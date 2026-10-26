//! `core/xml/xpath_eval.rs` -- XPath 1.0 evaluation over the arena DOM (L14 cause C09).
//!
//! Pure Rust over `&Dom`: an [`Expr`] and a context node in, a [`Val`] out. Where XPath 1.0 leaves
//! the DOM mapping open, the answer is Blink's (`third_party/blink/renderer/core/xml/xpath_*.cc`),
//! checked against google-chrome 150.0.7871.46 on a loopback page:
//!
//! ```text
//! name tests, HTML document    an HTML element matches an unprefixed name case-INsensitively (//LI);
//!                              a non-HTML element (inline <svg>) matches only a prefixed test
//! attribute names, HTML elem   case-insensitive (//*[@CLASS] finds class="...")
//! node()                       matches every DOM node, a doctype included (//node() counts it)
//! namespace axis               empty (Blink implements no namespace nodes)
//! "/" from a detached node     the root of the node's own tree (name(/*) of a <div> in a fragment is div)
//! number -> string             ECMAScript toPrecision(6), trailing zeros cut only without an exponent:
//!                              1 div 3 = 0.333333, 1e21 = 1.00000e+21, 0.0000001 = 1.00000e-7, -0 = 0
//! string -> number             whitespace-simplified, digits/'.'/'-' only: " 12 " = 12, "1e2" = NaN, "+5" = NaN
//! string-length/substring      UTF-16 code units ("a" + one emoji has length 3)
//! a non-node-set where a node-set is required (1[1], (1)/a, 1 | //li, count(1))
//!                              "Type conversion failed while evaluating the expression." (SyntaxError)
//! ```

use std::collections::HashMap;

use korvus_dom::{Dom, NodeId, NodeKind};

use super::xpath_parse::{ArithOp, Axis, Expr, Func, NodeTest, RelOp, Step};

const NS_XHTML: &str = "http://www.w3.org/1999/xhtml";
const NS_XML: &str = "http://www.w3.org/XML/1998/namespace";

/// An XPath node: an arena node, or an attribute (owner element, index into its attribute list).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum XNode {
    Node(NodeId),
    Attr(NodeId, usize),
}

#[derive(Clone, Debug)]
pub(crate) enum Val {
    Set(Vec<XNode>),
    Num(f64),
    Str(String),
    Bool(bool),
}

/// The one evaluation error: a value that had to be a node-set was not.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct TypeConversion;

type R<T> = Result<T, TypeConversion>;

/// Everything evaluation reads: the arena, the qualified names `createElementNS` recorded, and the
/// per-tree document-order index built on first need.
pub(crate) struct Ctx<'d> {
    pub dom: &'d Dom,
    pub qnames: &'d HashMap<u32, (Option<String>, String)>,
    order: HashMap<NodeId, HashMap<NodeId, usize>>,
}

#[derive(Clone, Copy)]
struct Focus {
    node: XNode,
    pos: usize,
    size: usize,
}

impl<'d> Ctx<'d> {
    pub(crate) fn new(dom: &'d Dom, qnames: &'d HashMap<u32, (Option<String>, String)>) -> Self {
        Ctx { dom, qnames, order: HashMap::new() }
    }

    /// Evaluate `e` with `node` as the context node (position 1 of 1).
    pub(crate) fn evaluate(&mut self, e: &Expr, node: XNode) -> R<Val> {
        self.eval(e, Focus { node, pos: 1, size: 1 })
    }

    // ── node facts ────────────────────────────────────────────────────────────────────────────────

    fn kind(&self, id: NodeId) -> &NodeKind {
        &self.dom.node(id).kind
    }

    fn element_of(&self, n: XNode) -> NodeId {
        match n {
            XNode::Node(id) | XNode::Attr(id, _) => id,
        }
    }

    /// `(prefix, localName)` of an element, from the recorded qualified name or the tag.
    fn element_qname(&self, id: NodeId) -> (Option<String>, String) {
        if let Some((p, l)) = self.qnames.get(&id) {
            return (p.clone(), l.clone());
        }
        let name = self.dom.tag_name(id).unwrap_or("").to_string();
        match name.split_once(':') {
            Some((p, l)) if self.dom.is_in_xml(id) && !p.is_empty() && !l.is_empty() => (Some(p.to_string()), l.to_string()),
            _ => (None, name),
        }
    }

    fn attrs(&self, id: NodeId) -> &[(String, String)] {
        match self.kind(id) {
            NodeKind::Element { attrs, .. } => attrs,
            _ => &[],
        }
    }

    /// `(prefix, localName, namespaceURI)` of an attribute.
    fn attr_qname(&self, id: NodeId, i: usize) -> (Option<String>, String, Option<String>) {
        let name = self.attrs(id).get(i).map(|(k, _)| k.as_str()).unwrap_or("");
        match name.split_once(':') {
            Some(("xml", l)) => (Some("xml".into()), l.to_string(), Some(NS_XML.into())),
            _ => (None, name.to_string(), None),
        }
    }

    /// An `xmlns` / `xmlns:*` declaration is not an attribute node in XPath.
    fn is_xmlns(name: &str) -> bool {
        name == "xmlns" || name.starts_with("xmlns:")
    }

    fn is_html_element_in_html_doc(&self, id: NodeId) -> bool {
        matches!(self.kind(id), NodeKind::Element { .. })
            && !self.dom.is_in_xml(id)
            && self.dom.namespace_uri(id) == Some(NS_XHTML)
    }

    fn parent(&self, n: XNode) -> Option<XNode> {
        match n {
            XNode::Attr(owner, _) => Some(XNode::Node(owner)),
            XNode::Node(id) => self.dom.parent(id).map(XNode::Node),
        }
    }

    /// The XPath string-value of a node.
    pub(crate) fn string_value(&self, n: XNode) -> String {
        match n {
            XNode::Attr(id, i) => self.attrs(id).get(i).map(|(_, v)| v.clone()).unwrap_or_default(),
            XNode::Node(id) => match self.kind(id) {
                NodeKind::Text(t) | NodeKind::Comment(t) | NodeKind::CData(t) => t.clone(),
                NodeKind::ProcessingInstruction(pi) => pi.data.clone(),
                NodeKind::Doctype(_) => String::new(),
                NodeKind::Element { .. } | NodeKind::Document | NodeKind::Fragment => {
                    let mut out = String::new();
                    self.collect_text(id, &mut out);
                    out
                }
            },
        }
    }

    fn collect_text(&self, id: NodeId, out: &mut String) {
        let mut c = self.dom.first_child(id);
        while let Some(cid) = c {
            match self.kind(cid) {
                NodeKind::Text(t) | NodeKind::CData(t) => out.push_str(t),
                NodeKind::Element { .. } => self.collect_text(cid, out),
                _ => {}
            }
            c = self.dom.next_sibling(cid);
        }
    }

    // ── document order ────────────────────────────────────────────────────────────────────────────

    fn order_key(&mut self, n: XNode) -> (NodeId, usize, usize) {
        let (id, slot) = match n {
            XNode::Node(id) => (id, 0),
            XNode::Attr(id, i) => (id, i + 1),
        };
        let root = self.dom.root_of(id);
        if !self.order.contains_key(&root) {
            let mut m = HashMap::new();
            let mut stack = vec![root];
            let mut idx = 0usize;
            while let Some(cur) = stack.pop() {
                m.insert(cur, idx);
                idx += 1;
                let mut kids: Vec<NodeId> = Vec::new();
                let mut c = self.dom.first_child(cur);
                while let Some(k) = c {
                    kids.push(k);
                    c = self.dom.next_sibling(k);
                }
                stack.extend(kids.into_iter().rev());
            }
            self.order.insert(root, m);
        }
        let pos = self.order.get(&root).and_then(|m| m.get(&id)).copied().unwrap_or(usize::MAX);
        (root, pos, slot)
    }

    /// Sort into document order and drop duplicates.
    fn sort_unique(&mut self, nodes: Vec<XNode>) -> Vec<XNode> {
        let mut keyed: Vec<((NodeId, usize, usize), XNode)> = nodes.into_iter().map(|n| (self.order_key(n), n)).collect();
        keyed.sort_by(|a, b| a.0.cmp(&b.0));
        keyed.dedup_by(|a, b| a.1 == b.1);
        keyed.into_iter().map(|(_, n)| n).collect()
    }

    // ── axes ──────────────────────────────────────────────────────────────────────────────────────

    fn push_descendants(&self, id: NodeId, out: &mut Vec<XNode>) {
        let mut c = self.dom.first_child(id);
        while let Some(k) = c {
            out.push(XNode::Node(k));
            self.push_descendants(k, out);
            c = self.dom.next_sibling(k);
        }
    }

    /// The next node in document order after `id`, skipping its children, bounded by the tree.
    fn next_skipping_children(&self, id: NodeId) -> Option<NodeId> {
        let mut cur = id;
        loop {
            if let Some(s) = self.dom.next_sibling(cur) {
                return Some(s);
            }
            cur = self.dom.parent(cur)?;
        }
    }

    fn next_in_order(&self, id: NodeId) -> Option<NodeId> {
        self.dom.first_child(id).or_else(|| self.next_skipping_children(id))
    }

    /// The previous node in document order (Blink's `NodeTraversal::Previous`).
    fn previous_in_order(&self, id: NodeId) -> Option<NodeId> {
        match self.dom.previous_sibling(id) {
            Some(mut p) => {
                while let Some(l) = self.dom.last_child(p) {
                    p = l;
                }
                Some(p)
            }
            None => self.dom.parent(id),
        }
    }

    /// The nodes of `axis` from `n`, in AXIS order (nearest-first for a reverse axis).
    fn axis_nodes(&self, axis: Axis, n: XNode) -> Vec<XNode> {
        let mut out = Vec::new();
        match axis {
            Axis::Child => {
                if let XNode::Node(id) = n {
                    let mut c = self.dom.first_child(id);
                    while let Some(k) = c {
                        out.push(XNode::Node(k));
                        c = self.dom.next_sibling(k);
                    }
                }
            }
            Axis::Descendant => {
                if let XNode::Node(id) = n {
                    self.push_descendants(id, &mut out);
                }
            }
            Axis::DescendantOrSelf => {
                out.push(n);
                if let XNode::Node(id) = n {
                    self.push_descendants(id, &mut out);
                }
            }
            Axis::Parent => out.extend(self.parent(n)),
            Axis::Ancestor | Axis::AncestorOrSelf => {
                if axis == Axis::AncestorOrSelf {
                    out.push(n);
                }
                let mut cur = self.parent(n);
                while let Some(p) = cur {
                    out.push(p);
                    cur = self.parent(p);
                }
            }
            Axis::SelfAxis => out.push(n),
            Axis::FollowingSibling | Axis::PrecedingSibling => {
                if let XNode::Node(id) = n {
                    let mut cur = if axis == Axis::FollowingSibling { self.dom.next_sibling(id) } else { self.dom.previous_sibling(id) };
                    while let Some(s) = cur {
                        out.push(XNode::Node(s));
                        cur = if axis == Axis::FollowingSibling { self.dom.next_sibling(s) } else { self.dom.previous_sibling(s) };
                    }
                }
            }
            Axis::Following => {
                // An attribute's following axis starts INSIDE its owner (Blink: `Next(ownerElement)`).
                let mut cur = match n {
                    XNode::Attr(owner, _) => self.next_in_order(owner),
                    XNode::Node(id) => self.next_skipping_children(id),
                };
                while let Some(k) = cur {
                    out.push(XNode::Node(k));
                    cur = self.next_in_order(k);
                }
            }
            Axis::Preceding => {
                let start = self.element_of(n);
                let mut node = start;
                while let Some(parent) = self.dom.parent(node) {
                    let mut p = self.previous_in_order(node);
                    while let Some(prev) = p {
                        if prev == parent {
                            break;
                        }
                        out.push(XNode::Node(prev));
                        p = self.previous_in_order(prev);
                    }
                    node = parent;
                }
            }
            Axis::Attribute => {
                if let XNode::Node(id) = n {
                    for (i, (k, _)) in self.attrs(id).iter().enumerate() {
                        if !Self::is_xmlns(k) {
                            out.push(XNode::Attr(id, i));
                        }
                    }
                }
            }
            Axis::Namespace => {}
        }
        out
    }

    fn matches(&self, axis: Axis, test: &NodeTest, n: XNode) -> bool {
        match test {
            NodeTest::AnyNode => true,
            NodeTest::Text => matches!(n, XNode::Node(id) if matches!(self.kind(id), NodeKind::Text(_) | NodeKind::CData(_))),
            NodeTest::Comment => matches!(n, XNode::Node(id) if matches!(self.kind(id), NodeKind::Comment(_))),
            NodeTest::Pi(target) => match n {
                XNode::Node(id) => match self.kind(id) {
                    NodeKind::ProcessingInstruction(pi) => target.as_ref().is_none_or(|want| *want == pi.target),
                    _ => false,
                },
                XNode::Attr(..) => false,
            },
            NodeTest::Name { ns, local } => {
                if axis == Axis::Namespace {
                    return false;
                }
                if axis == Axis::Attribute {
                    let XNode::Attr(owner, i) = n else { return false };
                    let (_, alocal, ans) = self.attr_qname(owner, i);
                    if local == "*" {
                        return ns.is_none() || *ns == ans;
                    }
                    let name_ok = if self.is_html_element_in_html_doc(owner) {
                        local.to_ascii_lowercase() == alocal
                    } else {
                        *local == alocal
                    };
                    return name_ok && *ns == ans;
                }
                let XNode::Node(id) = n else { return false };
                if !matches!(self.kind(id), NodeKind::Element { .. }) {
                    return false;
                }
                let el_ns = self.dom.namespace_uri(id).map(str::to_string);
                if local == "*" {
                    return ns.is_none() || *ns == el_ns;
                }
                let (_, el_local) = self.element_qname(id);
                if !self.dom.is_in_xml(id) {
                    if el_ns.as_deref() == Some(NS_XHTML) {
                        return el_local.eq_ignore_ascii_case(local) && (ns.is_none() || *ns == el_ns);
                    }
                    return el_local == *local && ns.is_some() && *ns == el_ns;
                }
                el_local == *local && *ns == el_ns
            }
        }
    }

    // ── evaluation ────────────────────────────────────────────────────────────────────────────────

    fn eval_steps(&mut self, mut nodes: Vec<XNode>, steps: &[Step]) -> R<Vec<XNode>> {
        for step in steps {
            let mut next: Vec<XNode> = Vec::new();
            for &n in &nodes {
                let mut cand: Vec<XNode> = self.axis_nodes(step.axis, n).into_iter().filter(|&c| self.matches(step.axis, &step.test, c)).collect();
                for pred in &step.preds {
                    cand = self.filter(cand, pred)?;
                }
                next.extend(cand);
            }
            nodes = self.sort_unique(next);
        }
        Ok(nodes)
    }

    /// Apply one predicate to `nodes`, whose order defines the proximity positions.
    fn filter(&mut self, nodes: Vec<XNode>, pred: &Expr) -> R<Vec<XNode>> {
        let size = nodes.len();
        let mut out = Vec::with_capacity(size);
        for (i, n) in nodes.into_iter().enumerate() {
            let f = Focus { node: n, pos: i + 1, size };
            let keep = match self.eval(pred, f)? {
                Val::Num(x) => x == (i + 1) as f64,
                v => self.to_bool(&v),
            };
            if keep {
                out.push(n);
            }
        }
        Ok(out)
    }

    fn eval(&mut self, e: &Expr, f: Focus) -> R<Val> {
        Ok(match e {
            Expr::Or(a, b) => {
                let l = self.eval(a, f)?;
                if self.to_bool(&l) {
                    Val::Bool(true)
                } else {
                    let r = self.eval(b, f)?;
                    Val::Bool(self.to_bool(&r))
                }
            }
            Expr::And(a, b) => {
                let l = self.eval(a, f)?;
                if !self.to_bool(&l) {
                    Val::Bool(false)
                } else {
                    let r = self.eval(b, f)?;
                    Val::Bool(self.to_bool(&r))
                }
            }
            Expr::Eq(ne, a, b) => {
                let (l, r) = (self.eval(a, f)?, self.eval(b, f)?);
                Val::Bool(self.compare(Cmp::Eq(*ne), &l, &r))
            }
            Expr::Rel(op, a, b) => {
                let (l, r) = (self.eval(a, f)?, self.eval(b, f)?);
                Val::Bool(self.compare(Cmp::Rel(*op), &l, &r))
            }
            Expr::Arith(op, a, b) => {
                let (l, r) = (self.eval(a, f)?, self.eval(b, f)?);
                let (x, y) = (self.to_num(&l), self.to_num(&r));
                Val::Num(match op {
                    ArithOp::Add => x + y,
                    ArithOp::Sub => x - y,
                    ArithOp::Mul => x * y,
                    ArithOp::Div => x / y,
                    ArithOp::Mod => x % y,
                })
            }
            Expr::Neg(a) => {
                let v = self.eval(a, f)?;
                Val::Num(-self.to_num(&v))
            }
            Expr::Union(a, b) => {
                let (Val::Set(mut l), Val::Set(r)) = (self.eval(a, f)?, self.eval(b, f)?) else { return Err(TypeConversion) };
                l.extend(r);
                Val::Set(self.sort_unique(l))
            }
            Expr::Path { absolute, steps } => {
                let start = if *absolute { XNode::Node(self.dom.root_of(self.element_of(f.node))) } else { f.node };
                Val::Set(self.eval_steps(vec![start], steps)?)
            }
            Expr::Filter { primary, preds, steps } => {
                let Val::Set(nodes) = self.eval(primary, f)? else { return Err(TypeConversion) };
                let mut nodes = self.sort_unique(nodes);
                for p in preds {
                    nodes = self.filter(nodes, p)?;
                }
                Val::Set(self.eval_steps(nodes, steps)?)
            }
            Expr::Literal(s) => Val::Str(s.clone()),
            Expr::Number(n) => Val::Num(*n),
            // Blink binds no variables for a page; an unbound reference is the empty string.
            Expr::Var(_) => Val::Str(String::new()),
            Expr::Call(func, args) => self.call(*func, args, f)?,
        })
    }

    fn arg_set(&mut self, e: &Expr, f: Focus) -> R<Vec<XNode>> {
        match self.eval(e, f)? {
            Val::Set(s) => Ok(self.sort_unique(s)),
            _ => Err(TypeConversion),
        }
    }

    fn arg_str(&mut self, args: &[Expr], i: usize, f: Focus) -> R<String> {
        match args.get(i) {
            Some(e) => {
                let v = self.eval(e, f)?;
                Ok(self.to_str(&v))
            }
            None => Ok(self.string_value(f.node)),
        }
    }

    fn arg_num(&mut self, e: &Expr, f: Focus) -> R<f64> {
        let v = self.eval(e, f)?;
        Ok(self.to_num(&v))
    }

    /// The node an optional node-set argument names (its first in document order), or the context.
    fn arg_node(&mut self, args: &[Expr], f: Focus) -> R<Option<XNode>> {
        match args.first() {
            Some(e) => Ok(self.arg_set(e, f)?.first().copied()),
            None => Ok(Some(f.node)),
        }
    }

    fn call(&mut self, func: Func, args: &[Expr], f: Focus) -> R<Val> {
        Ok(match func {
            Func::Last => Val::Num(f.size as f64),
            Func::Position => Val::Num(f.pos as f64),
            Func::Count => Val::Num(self.arg_set(&args[0], f)?.len() as f64),
            Func::Id => {
                let v = self.eval(&args[0], f)?;
                let text = match &v {
                    Val::Set(s) => s.iter().map(|&n| self.string_value(n)).collect::<Vec<_>>().join(" "),
                    other => self.to_str(other),
                };
                let root = self.dom.root_of(self.element_of(f.node));
                let mut out = Vec::new();
                for token in text.split(|c: char| is_xml_space(c)).filter(|t| !t.is_empty()) {
                    if let Some(id) = self.dom.get_element_by_id_in(root, token) {
                        out.push(XNode::Node(id));
                    }
                }
                Val::Set(self.sort_unique(out))
            }
            Func::LocalName | Func::NamespaceUri | Func::Name => {
                let Some(n) = self.arg_node(args, f)? else { return Ok(Val::Str(String::new())) };
                let (prefix, local, ns) = match n {
                    XNode::Attr(owner, i) => self.attr_qname(owner, i),
                    XNode::Node(id) => match self.kind(id) {
                        NodeKind::Element { .. } => {
                            let (p, l) = self.element_qname(id);
                            (p, l, self.dom.namespace_uri(id).map(str::to_string))
                        }
                        NodeKind::ProcessingInstruction(pi) => (None, pi.target.clone(), None),
                        _ => (None, String::new(), None),
                    },
                };
                Val::Str(match func {
                    Func::LocalName => local,
                    Func::NamespaceUri => ns.unwrap_or_default(),
                    _ => match prefix {
                        Some(p) if !p.is_empty() => format!("{p}:{local}"),
                        _ => local,
                    },
                })
            }
            Func::String => Val::Str(self.arg_str(args, 0, f)?),
            Func::Concat => {
                let mut s = String::new();
                for i in 0..args.len() {
                    s.push_str(&self.arg_str(args, i, f)?);
                }
                Val::Str(s)
            }
            Func::StartsWith => {
                let (a, b) = (self.arg_str(args, 0, f)?, self.arg_str(args, 1, f)?);
                Val::Bool(a.starts_with(&b))
            }
            Func::Contains => {
                let (a, b) = (self.arg_str(args, 0, f)?, self.arg_str(args, 1, f)?);
                Val::Bool(a.contains(&b))
            }
            Func::SubstringBefore => {
                let (a, b) = (self.arg_str(args, 0, f)?, self.arg_str(args, 1, f)?);
                Val::Str(a.find(&b).map(|i| a[..i].to_string()).unwrap_or_default())
            }
            Func::SubstringAfter => {
                let (a, b) = (self.arg_str(args, 0, f)?, self.arg_str(args, 1, f)?);
                Val::Str(a.find(&b).map(|i| a[i + b.len()..].to_string()).unwrap_or_default())
            }
            Func::Substring => {
                let s: Vec<u16> = self.arg_str(args, 0, f)?.encode_utf16().collect();
                let start = xpath_round(self.arg_num(&args[1], f)?);
                let len = match args.get(2) {
                    Some(e) => xpath_round(self.arg_num(e, f)?),
                    None => f64::INFINITY,
                };
                Val::Str(substring_units(&s, start, len))
            }
            Func::StringLength => Val::Num(self.arg_str(args, 0, f)?.encode_utf16().count() as f64),
            Func::NormalizeSpace => Val::Str(simplify_whitespace(&self.arg_str(args, 0, f)?)),
            Func::Translate => {
                let s: Vec<u16> = self.arg_str(args, 0, f)?.encode_utf16().collect();
                let from: Vec<u16> = self.arg_str(args, 1, f)?.encode_utf16().collect();
                let to: Vec<u16> = self.arg_str(args, 2, f)?.encode_utf16().collect();
                let mut out: Vec<u16> = Vec::with_capacity(s.len());
                for u in s {
                    match from.iter().position(|&x| x == u) {
                        None => out.push(u),
                        Some(i) => {
                            if let Some(&t) = to.get(i) {
                                out.push(t);
                            }
                        }
                    }
                }
                Val::Str(String::from_utf16_lossy(&out))
            }
            Func::Boolean => {
                let v = self.eval(&args[0], f)?;
                Val::Bool(self.to_bool(&v))
            }
            Func::Not => {
                let v = self.eval(&args[0], f)?;
                Val::Bool(!self.to_bool(&v))
            }
            Func::True => Val::Bool(true),
            Func::False => Val::Bool(false),
            Func::Lang => {
                let want = self.arg_str(args, 0, f)?.to_ascii_lowercase();
                let mut cur = Some(XNode::Node(self.element_of(f.node)));
                let mut found: Option<String> = None;
                while let Some(n) = cur {
                    if let XNode::Node(id) = n {
                        if let Some((_, v)) = self.attrs(id).iter().find(|(k, _)| k == "xml:lang") {
                            found = Some(v.to_ascii_lowercase());
                            break;
                        }
                    }
                    cur = self.parent(n);
                }
                Val::Bool(match found {
                    Some(l) => l == want || (l.starts_with(&want) && l[want.len()..].starts_with('-')),
                    None => false,
                })
            }
            Func::Number => match args.first() {
                Some(e) => Val::Num(self.arg_num(e, f)?),
                None => Val::Num(string_to_number(&self.string_value(f.node))),
            },
            Func::Sum => {
                let set = self.arg_set(&args[0], f)?;
                Val::Num(set.iter().map(|&n| string_to_number(&self.string_value(n))).sum())
            }
            Func::Floor => Val::Num(self.arg_num(&args[0], f)?.floor()),
            Func::Ceiling => Val::Num(self.arg_num(&args[0], f)?.ceil()),
            Func::Round => Val::Num(xpath_round(self.arg_num(&args[0], f)?)),
        })
    }

    // ── conversions and comparison ────────────────────────────────────────────────────────────────

    pub(crate) fn to_bool(&self, v: &Val) -> bool {
        match v {
            Val::Set(s) => !s.is_empty(),
            Val::Num(n) => *n != 0.0 && !n.is_nan(),
            Val::Str(s) => !s.is_empty(),
            Val::Bool(b) => *b,
        }
    }

    pub(crate) fn to_num(&mut self, v: &Val) -> f64 {
        match v {
            Val::Num(n) => *n,
            Val::Bool(b) => f64::from(u8::from(*b)),
            Val::Str(s) => string_to_number(s),
            Val::Set(_) => string_to_number(&self.to_str(v)),
        }
    }

    pub(crate) fn to_str(&mut self, v: &Val) -> String {
        match v {
            Val::Str(s) => s.clone(),
            Val::Bool(b) => if *b { "true".into() } else { "false".into() },
            Val::Num(n) => number_to_string(*n),
            Val::Set(s) => {
                let sorted = self.sort_unique(s.clone());
                sorted.first().map(|&n| self.string_value(n)).unwrap_or_default()
            }
        }
    }

    fn compare(&mut self, op: Cmp, l: &Val, r: &Val) -> bool {
        match (l, r) {
            (Val::Set(ls), Val::Set(rs)) => {
                let rvals: Vec<String> = rs.iter().map(|&n| self.string_value(n)).collect();
                ls.iter().any(|&a| {
                    let av = Val::Str(self.string_value(a));
                    rvals.iter().any(|b| compare_atoms(op, &av, &Val::Str(b.clone())))
                })
            }
            (Val::Set(ls), other) | (other, Val::Set(ls)) => {
                let set_on_left = matches!(l, Val::Set(_));
                if let Val::Bool(_) = other {
                    let b = Val::Bool(!ls.is_empty());
                    return if set_on_left { compare_atoms(op, &b, other) } else { compare_atoms(op, other, &b) };
                }
                ls.iter().any(|&n| {
                    let sv = self.string_value(n);
                    let atom = match other {
                        Val::Num(_) => Val::Num(string_to_number(&sv)),
                        _ => Val::Str(sv),
                    };
                    if set_on_left { compare_atoms(op, &atom, other) } else { compare_atoms(op, other, &atom) }
                })
            }
            _ => compare_atoms(op, l, r),
        }
    }
}

#[derive(Clone, Copy)]
enum Cmp {
    Eq(bool),
    Rel(RelOp),
}

fn atom_num(v: &Val) -> f64 {
    match v {
        Val::Num(n) => *n,
        Val::Bool(b) => f64::from(u8::from(*b)),
        Val::Str(s) => string_to_number(s),
        Val::Set(_) => f64::NAN,
    }
}

fn atom_str(v: &Val) -> String {
    match v {
        Val::Str(s) => s.clone(),
        Val::Num(n) => number_to_string(*n),
        Val::Bool(b) => if *b { "true".into() } else { "false".into() },
        Val::Set(_) => String::new(),
    }
}

fn atom_bool(v: &Val) -> bool {
    match v {
        Val::Bool(b) => *b,
        Val::Num(n) => *n != 0.0 && !n.is_nan(),
        Val::Str(s) => !s.is_empty(),
        Val::Set(s) => !s.is_empty(),
    }
}

/// §3.4 for two non-node-set values.
fn compare_atoms(op: Cmp, l: &Val, r: &Val) -> bool {
    match op {
        Cmp::Eq(ne) => {
            let eq = if matches!(l, Val::Bool(_)) || matches!(r, Val::Bool(_)) {
                atom_bool(l) == atom_bool(r)
            } else if matches!(l, Val::Num(_)) || matches!(r, Val::Num(_)) {
                atom_num(l) == atom_num(r)
            } else {
                atom_str(l) == atom_str(r)
            };
            // `!=` is not `not(=)`: NaN != NaN is true, and so is NaN = NaN false.
            if ne {
                if matches!(l, Val::Bool(_)) || matches!(r, Val::Bool(_)) {
                    atom_bool(l) != atom_bool(r)
                } else if matches!(l, Val::Num(_)) || matches!(r, Val::Num(_)) {
                    atom_num(l) != atom_num(r)
                } else {
                    atom_str(l) != atom_str(r)
                }
            } else {
                eq
            }
        }
        Cmp::Rel(o) => {
            let (x, y) = (atom_num(l), atom_num(r));
            match o {
                RelOp::Lt => x < y,
                RelOp::Le => x <= y,
                RelOp::Gt => x > y,
                RelOp::Ge => x >= y,
            }
        }
    }
}

fn is_xml_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

/// Trim and collapse runs of whitespace to one space (`normalize-space`, and Blink's number parse).
fn simplify_whitespace(s: &str) -> String {
    s.split(is_xml_space).filter(|t| !t.is_empty()).collect::<Vec<_>>().join(" ")
}

/// XPath `number(string)` as Blink does it: simplified whitespace, then only digits, `.` and `-`.
pub(crate) fn string_to_number(s: &str) -> f64 {
    let t = simplify_whitespace(s);
    if t.is_empty() || !t.chars().all(|c| c.is_ascii_digit() || c == '.' || c == '-') {
        return f64::NAN;
    }
    t.parse::<f64>().unwrap_or(f64::NAN)
}

/// Blink's `FunRound::Round`: half-up, with a negative value in [-0.5, 0) rounding to -0.
pub(crate) fn xpath_round(x: f64) -> f64 {
    if x.is_nan() || x.is_infinite() {
        return x;
    }
    if x.is_sign_negative() && x >= -0.5 {
        return -0.0;
    }
    (x + 0.5).floor()
}

fn substring_units(s: &[u16], start: f64, len: f64) -> String {
    if start.is_nan() || len.is_nan() {
        return String::new();
    }
    let end = start + len;
    if end.is_nan() {
        return String::new();
    }
    let first = start.max(1.0);
    let last = end.min(s.len() as f64 + 1.0);
    if last <= first {
        return String::new();
    }
    String::from_utf16_lossy(&s[(first as usize - 1)..(last as usize - 1)])
}

/// Blink's `XPathValue::ToString` for a number: `NaN`, `0` (for either zero), `±Infinity`, and
/// otherwise `NumberToFixedPrecisionString(x, 6)` -- ECMAScript `toPrecision(6)`, trailing zeros cut
/// only when there is no exponent.
pub(crate) fn number_to_string(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x == 0.0 {
        return "0".into();
    }
    if x.is_infinite() {
        return if x < 0.0 { "-Infinity".into() } else { "Infinity".into() };
    }
    let neg = x < 0.0;
    // Exact decimal digits of |x| (Rust prints the exact expansion to the requested precision), then
    // ECMAScript's rounding: to six significant digits, ties to the LARGER magnitude.
    let sci = format!("{:.40e}", x.abs());
    let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let mut exp: i32 = exp.parse().unwrap_or(0);
    let digits: Vec<u8> = mant.bytes().filter(|b| b.is_ascii_digit()).map(|b| b - b'0').collect();
    let mut six: Vec<u8> = digits[..6].to_vec();
    if digits[6] >= 5 {
        let mut i = 5;
        loop {
            if six[i] == 9 {
                six[i] = 0;
                if i == 0 {
                    six.insert(0, 1);
                    six.truncate(6);
                    exp += 1;
                    break;
                }
                i -= 1;
            } else {
                six[i] += 1;
                break;
            }
        }
    }
    let ds: String = six.iter().map(|d| char::from(b'0' + d)).collect();
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if !(-6..6).contains(&exp) {
        out.push_str(&ds[..1]);
        out.push('.');
        out.push_str(&ds[1..]);
        out.push('e');
        out.push(if exp < 0 { '-' } else { '+' });
        out.push_str(&exp.abs().to_string());
        return out;
    }
    let body = if exp >= 0 {
        let e = exp as usize;
        if e == 5 { ds.clone() } else { format!("{}.{}", &ds[..e + 1], &ds[e + 1..]) }
    } else {
        format!("0.{}{}", "0".repeat((-exp - 1) as usize), ds)
    };
    let body = if body.contains('.') { body.trim_end_matches('0').trim_end_matches('.').to_string() } else { body };
    out.push_str(&body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::xml::xpath_parse::parse;

    #[test]
    fn numbers_print_the_way_chrome_150_printed_them() {
        for (x, want) in [
            (1.0 / 3.0, "0.333333"),
            (1e21, "1.00000e+21"),
            (1e22, "1.00000e+22"),
            (0.0000001, "1.00000e-7"),
            (0.000001, "0.000001"),
            (-0.0, "0"),
            (-0.5, "-0.5"),
            (123456789012345678.0, "1.23457e+17"),
            (0.1 + 0.2, "0.3"),
            (1.0, "1"),
            (-3.5, "-3.5"),
            (6.0 / 7.0, "0.857143"),
            (100.0, "100"),
            (123456.0, "123456"),
            (100000.5, "100001"),
            (999999.5, "1.00000e+6"),
            (f64::INFINITY, "Infinity"),
            (f64::NEG_INFINITY, "-Infinity"),
            (f64::NAN, "NaN"),
        ] {
            assert_eq!(number_to_string(x), want, "{x}");
        }
    }

    #[test]
    fn strings_parse_to_numbers_the_way_chrome_150_parsed_them() {
        assert_eq!(string_to_number(" 12 "), 12.0);
        assert!(string_to_number("1 2").is_nan());
        assert!(string_to_number("1e2").is_nan());
        assert_eq!(string_to_number(".5"), 0.5);
        assert_eq!(string_to_number("5."), 5.0);
        assert!(string_to_number("").is_nan());
        assert_eq!(string_to_number("-.5"), -0.5);
        assert!(string_to_number("+5").is_nan());
        assert!(string_to_number(".").is_nan());
    }

    #[test]
    fn round_and_substring_follow_chrome_150() {
        assert_eq!(number_to_string(xpath_round(-0.5)), "0");
        assert_eq!(xpath_round(2.5), 3.0);
        assert_eq!(xpath_round(-2.5), -2.0);
        let s: Vec<u16> = "12345".encode_utf16().collect();
        assert_eq!(substring_units(&s, xpath_round(1.5), xpath_round(2.6)), "234");
        assert_eq!(substring_units(&s, 0.0, 3.0), "12");
        assert_eq!(substring_units(&s, f64::NAN, 3.0), "");
        assert_eq!(substring_units(&s, -42.0, f64::INFINITY), "12345");
        assert_eq!(substring_units(&s, f64::NEG_INFINITY, f64::INFINITY), "");
    }

    fn eval_str(html: &str, expr: &str) -> String {
        let dom = Dom::parse(html);
        let q = HashMap::new();
        let mut cx = Ctx::new(&dom, &q);
        let e = parse(expr, &mut |_| Ok(Some(NS_XHTML.to_string()))).unwrap();
        let v = cx.evaluate(&e, XNode::Node(dom.root())).unwrap();
        cx.to_str(&v)
    }

    #[test]
    fn a_document_answers_chromes_strings() {
        let html = "<!doctype html><html lang=\"en\"><head><title>xp</title></head><body><!--c1--><ul id=\"l\"><li class=\"a\" n=\"1\">one</li><li class=\"b\" n=\"2\">two</li><li class=\"b\" n=\"3\">three</li></ul>\n</body></html>";
        for (x, want) in [
            ("substring-before(\"1999/04/01\",\"/\")", "1999"),
            ("substring-after(\"1999/04/01\",\"19\")", "99/04/01"),
            ("translate(\"--aaa--\",\"abc-\",\"ABC\")", "AAA"),
            ("normalize-space(\"  a   b \")", "a b"),
            ("name(//li)", "li"),
            ("namespace-uri(//li)", NS_XHTML),
            ("name(//li/@class)", "class"),
            ("name(/)", ""),
            ("string(//li/@n)", "1"),
            ("lang(\"en\")", "false"),
            ("string(id(\"l\")/li[2])", "two"),
            ("string(//ul/li[last()-1])", "two"),
            ("count(//li[@n > 1])", "2"),
            ("string(//LI)", "one"),
            ("count(//html:li)", "3"),
            ("count(//li/ancestor::*)", "3"),
            ("string(//li[3]/preceding-sibling::li[1])", "two"),
            ("string((//li[3]/preceding-sibling::li)[1])", "one"),
            ("count(//li/preceding::*)", "4"),
            ("string(//comment())", "c1"),
            ("//li[2] = \"two\"", "true"),
            ("//li != //li", "true"),
            ("//li < 2", "false"),
            ("true() = \"x\"", "true"),
            ("\"1\" = 1", "true"),
            ("2 > \"10\"", "false"),
            ("count(//*[@CLASS])", "3"),
            ("7 mod -3", "1"),
            ("-7 mod 3", "-1"),
        ] {
            assert_eq!(eval_str(html, x), want, "{x}");
        }
    }
}

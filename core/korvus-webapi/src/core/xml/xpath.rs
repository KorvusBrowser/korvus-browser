//! `core/xml/xpath.rs` -- `XPathEvaluator`, `XPathExpression`, `XPathResult`, and the three `Document`
//! members that share their bodies (L14 cause C09).
//!
//! WHAT WAS HERE BEFORE: nothing. All three interfaces were generated-registry shells whose every
//! member was a D2 stub, so `new XPathEvaluator().createExpression(".//*", null)` answered `undefined`,
//! `document.createExpression` / `document.evaluate` answered `null`, and a page that feature-detects
//! XPath and then uses it (three corpus sites did, per the L14 findings) threw on the result.
//!
//! The engine is [`super::xpath_parse`] + [`super::xpath_eval`]; this file is the WebIDL half, every
//! message and shape measured on google-chrome 150.0.7871.46 (loopback page):
//!
//! ```text
//! document.evaluate("//li", null)                 TypeError  Failed to execute 'evaluate' on 'Document': parameter 2 is not of type 'Node'.
//! document.evaluate(".", fragment | doctype)      NotSupportedError  ...: The node provided is '#document-fragment', which is not a valid context node type.
//! document.evaluate("1 +", document)              SyntaxError  ...: The string '1 +' is not a valid XPath expression.
//! document.evaluate("//x:li", document)           NamespaceError  ...: The string '//x:li' contains unresolvable namespaces.
//! evaluate("1", document, null, 4 | 7 | 9)        TypeError  ...: The result is not a node set, and therefore cannot be converted to the desired type.
//! evaluate("1[1]" | "(1)/a" | "1 | //li")         SyntaxError  ...: Type conversion failed while evaluating the expression.
//! result.stringValue on a number                  TypeError  Failed to read the 'stringValue' property from 'XPathResult': The result type is not a string.
//! result.iterateNext() after a DOM mutation       InvalidStateError  Failed to execute 'iterateNext' on 'XPathResult': The document has mutated since the result was returned.
//! snapshotItem(9) past the end                    null
//! evaluate(.., type 10 | 65535)                   an XPathResult (an unknown type is ANY)
//! evaluate(.., inResult)                          a NEW XPathResult, never inResult
//! createNSResolver(node)                          node itself
//! Object.getOwnPropertyNames(expr | result | evaluator)   []
//! ```
//!
//! A resolver is a function or an object with `lookupNamespaceURI`, asked once per prefix at parse
//! time; its exception propagates. Per-instance state (an expression's source and resolved prefixes,
//! a result's type, value, nodes and iterator position) lives in v8 privates, so no instance has an
//! own property.

use deno_core::v8;

use crate::binding::protos::{get_prop, make_dom_exc, str_val, throw_type_error, wrap_cached};
use crate::binding::{HostDom, QNameMap};
use crate::core::dom::attr::{attr_eid_of, attr_name_of, make_attr};
use crate::core::dom::node::{node_recv, node_recv_id, NodeRecv};
use crate::core::events::event::{evt_slot_read, evt_slot_write};
use crate::install::webidl::{brand, branded_getter_row, branded_method_row};
use crate::install::MemberDef;

use super::xpath_eval::{Ctx, TypeConversion, Val, XNode};
use super::xpath_parse::{parse, Expr, ParseError};

const RESULT: &str = "XPathResult";
const EXPRESSION: &str = "XPathExpression";
const EVALUATOR: &str = "XPathEvaluator";

const SLOT_SOURCE: &str = "korvus_xpe_source";
const SLOT_PREFIXES: &str = "korvus_xpe_prefixes";
const SLOT_TYPE: &str = "korvus_xpr_type";
const SLOT_VALUE: &str = "korvus_xpr_value";
const SLOT_NODES: &str = "korvus_xpr_nodes";
const SLOT_INDEX: &str = "korvus_xpr_index";
const SLOT_REVISION: &str = "korvus_xpr_revision";

const ANY_TYPE: u32 = 0;
const NUMBER_TYPE: u32 = 1;
const STRING_TYPE: u32 = 2;
const BOOLEAN_TYPE: u32 = 3;
const UNORDERED_NODE_ITERATOR_TYPE: u32 = 4;
const ORDERED_NODE_ITERATOR_TYPE: u32 = 5;
const UNORDERED_NODE_SNAPSHOT_TYPE: u32 = 6;
const ORDERED_NODE_SNAPSHOT_TYPE: u32 = 7;
const ANY_UNORDERED_NODE_TYPE: u32 = 8;
const FIRST_ORDERED_NODE_TYPE: u32 = 9;

fn dom_throw(scope: &mut v8::PinScope, msg: &str, name: &str) {
    if let Some(e) = make_dom_exc(scope, msg, name) {
        scope.throw_exception(e);
    }
}

// ── parsing with a resolver ─────────────────────────────────────────────────────────────────────────

/// Ask `resolver` for `prefix`: a callable is called, an object's `lookupNamespaceURI` is. `Ok(None)`
/// for null/undefined/"" or no resolver; `Err(())` when the resolver threw.
fn lookup_prefix(scope: &mut v8::PinScope, resolver: v8::Local<v8::Value>, prefix: &str) -> Result<Option<String>, ()> {
    if resolver.is_null_or_undefined() {
        return Ok(None);
    }
    let p = str_val(scope, prefix);
    let result = if let Ok(f) = v8::Local::<v8::Function>::try_from(resolver) {
        let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
        f.call(scope, undef, &[p])
    } else if let Ok(o) = v8::Local::<v8::Object>::try_from(resolver) {
        let method = get_prop(scope, o, "lookupNamespaceURI");
        match method.and_then(|m| v8::Local::<v8::Function>::try_from(m).ok()) {
            Some(f) => f.call(scope, o.into(), &[p]),
            None => return Ok(None),
        }
    } else {
        return Ok(None);
    };
    match result {
        None => Err(()),
        Some(v) if v.is_null_or_undefined() => Ok(None),
        Some(v) => {
            let s = v.to_rust_string_lossy(scope);
            Ok(if s.is_empty() { None } else { Some(s) })
        }
    }
}

/// Parse `src` for `method` on `iface`, resolving through `resolver` and recording every prefix it
/// resolved (so an `XPathExpression` can be re-parsed later without asking again). Throws Chrome's
/// error and returns `None` on failure.
fn compile(
    scope: &mut v8::PinScope,
    src: &str,
    resolver: v8::Local<v8::Value>,
    iface: &str,
    method: &str,
) -> Option<(Expr, Vec<(String, String)>)> {
    let mut resolved: Vec<(String, String)> = Vec::new();
    let r = {
        let mut f = |prefix: &str| -> Result<Option<String>, ()> {
            if let Some((_, u)) = resolved.iter().find(|(p, _)| p == prefix) {
                return Ok(Some(u.clone()));
            }
            let out = lookup_prefix(scope, resolver, prefix)?;
            if let Some(u) = &out {
                resolved.push((prefix.to_string(), u.clone()));
            }
            Ok(out)
        };
        parse(src, &mut f)
    };
    match r {
        Ok(e) => Some((e, resolved)),
        Err(ParseError::Syntax) => {
            dom_throw(scope, &format!("Failed to execute '{method}' on '{iface}': The string '{src}' is not a valid XPath expression."), "SyntaxError");
            None
        }
        Err(ParseError::Namespace) => {
            dom_throw(scope, &format!("Failed to execute '{method}' on '{iface}': The string '{src}' contains unresolvable namespaces."), "NamespaceError");
            None
        }
        Err(ParseError::Thrown) => None,
    }
}

// ── the context node ────────────────────────────────────────────────────────────────────────────────

/// The `Node contextNode` argument as an XPath node, or Chrome's TypeError / NotSupportedError.
fn context_node(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, iface: &str, method: &str, param: u32) -> Option<XNode> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let not_node = |scope: &mut v8::PinScope| {
        throw_type_error(scope, &format!("Failed to execute '{method}' on '{iface}': parameter {param} is not of type 'Node'."));
    };
    let r = node_recv(scope, v);
    let xn = match r {
        NodeRecv::Other => {
            let Ok(o) = v8::Local::<v8::Object>::try_from(v) else {
                not_node(scope);
                return None;
            };
            match (attr_eid_of(scope, o), attr_name_of(scope, o)) {
                (Some(eid), Some(name)) => {
                    let st = op_state.borrow();
                    let dom = &st.borrow::<HostDom>().0;
                    match dom.attribute_names(eid).iter().position(|n| *n == name) {
                        Some(i) => XNode::Attr(eid, i),
                        // A removed attribute's Attr still names its owner; XPath sees the owner.
                        None => XNode::Node(eid),
                    }
                }
                _ => {
                    not_node(scope);
                    return None;
                }
            }
        }
        _ => {
            let st = op_state.borrow();
            let dom = &st.borrow::<HostDom>().0;
            XNode::Node(node_recv_id(&r, dom).unwrap_or_else(|| dom.root()))
        }
    };
    if let XNode::Node(id) = xn {
        let (nt, name) = {
            let st = op_state.borrow();
            let dom = &st.borrow::<HostDom>().0;
            (dom.node_type(id), dom.node_name(id))
        };
        if matches!(nt, 10 | 11) {
            dom_throw(
                scope,
                &format!("Failed to execute '{method}' on '{iface}': The node provided is '{name}', which is not a valid context node type."),
                "NotSupportedError",
            );
            return None;
        }
    }
    Some(xn)
}

// ── evaluation into an XPathResult ──────────────────────────────────────────────────────────────────

/// Evaluate a compiled expression at `ctx` and mint the `XPathResult` for `type`, or throw.
fn evaluate_to_result<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    expr: &Expr,
    ctx: XNode,
    requested: u32,
    iface: &str,
    method: &str,
) -> Option<v8::Local<'s, v8::Object>> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // Evaluate and settle every conversion inside ONE borrow of the arena; no JS runs in here.
    enum Settled {
        Num(f64),
        Str(String),
        Bool(bool),
        Nodes(Vec<XNode>),
    }
    let (settled, rtype, revision) = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        let qn = st.try_borrow::<QNameMap>().map(|q| q.0.borrow());
        let empty = std::collections::HashMap::new();
        let qnames = match &qn {
            Some(r) => &**r,
            None => &empty,
        };
        let mut cx = Ctx::new(dom, qnames);
        let val = match cx.evaluate(expr, ctx) {
            Ok(v) => v,
            Err(TypeConversion) => {
                drop(qn);
                drop(st);
                dom_throw(scope, &format!("Failed to execute '{method}' on '{iface}': Type conversion failed while evaluating the expression."), "SyntaxError");
                return None;
            }
        };
        let natural = match &val {
            Val::Num(_) => NUMBER_TYPE,
            Val::Str(_) => STRING_TYPE,
            Val::Bool(_) => BOOLEAN_TYPE,
            Val::Set(_) => UNORDERED_NODE_ITERATOR_TYPE,
        };
        let rtype = if requested == ANY_TYPE || requested > FIRST_ORDERED_NODE_TYPE { natural } else { requested };
        let settled = match rtype {
            NUMBER_TYPE => Settled::Num(cx.to_num(&val)),
            STRING_TYPE => Settled::Str(cx.to_str(&val)),
            BOOLEAN_TYPE => Settled::Bool(cx.to_bool(&val)),
            _ => match val {
                Val::Set(nodes) => Settled::Nodes(nodes),
                _ => {
                    drop(qn);
                    drop(st);
                    throw_type_error(
                        scope,
                        &format!("Failed to execute '{method}' on '{iface}': The result is not a node set, and therefore cannot be converted to the desired type."),
                    );
                    return None;
                }
            },
        };
        (settled, rtype, dom.revision())
    };
    let obj = crate::install::live::mint_table(scope, RESULT);
    brand(scope, obj, RESULT);
    let t: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, rtype).into();
    evt_slot_write(scope, obj, SLOT_TYPE, t);
    match settled {
        Settled::Num(n) => {
            let v: v8::Local<v8::Value> = v8::Number::new(scope, n).into();
            evt_slot_write(scope, obj, SLOT_VALUE, v);
        }
        Settled::Str(s) => {
            let v = str_val(scope, &s);
            evt_slot_write(scope, obj, SLOT_VALUE, v);
        }
        Settled::Bool(b) => {
            let v: v8::Local<v8::Value> = v8::Boolean::new(scope, b).into();
            evt_slot_write(scope, obj, SLOT_VALUE, v);
        }
        Settled::Nodes(nodes) => {
            let keep = if matches!(rtype, ANY_UNORDERED_NODE_TYPE | FIRST_ORDERED_NODE_TYPE) { nodes.len().min(1) } else { nodes.len() };
            let arr = v8::Array::new(scope, keep as i32);
            for (i, n) in nodes.into_iter().take(keep).enumerate() {
                let v = node_value(scope, n);
                arr.set_index(scope, i as u32, v);
            }
            evt_slot_write(scope, obj, SLOT_NODES, arr.into());
            let zero: v8::Local<v8::Value> = v8::Integer::new(scope, 0).into();
            evt_slot_write(scope, obj, SLOT_INDEX, zero);
            if matches!(rtype, UNORDERED_NODE_ITERATOR_TYPE | ORDERED_NODE_ITERATOR_TYPE) {
                let rev: v8::Local<v8::Value> = v8::Number::new(scope, revision as f64).into();
                evt_slot_write(scope, obj, SLOT_REVISION, rev);
            }
        }
    }
    Some(obj)
}

/// The page object for an XPath node: the node's (interned) wrapper -- a document root is its
/// Document -- or a fresh `Attr` view.
fn node_value<'s>(scope: &mut v8::PinScope<'s, '_>, n: XNode) -> v8::Local<'s, v8::Value> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    match n {
        XNode::Node(id) => wrap_cached(scope, &op_state, id).into(),
        XNode::Attr(eid, i) => {
            let name = op_state.borrow().borrow::<HostDom>().0.attribute_names(eid).get(i).cloned().unwrap_or_default();
            match make_attr(scope, eid, &name) {
                Some(o) => o.into(),
                None => v8::null(scope).into(),
            }
        }
    }
}

/// WebIDL `unsigned short` for the `type` argument (undefined -> 0).
fn type_arg(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> u32 {
    if v.is_undefined() {
        return 0;
    }
    v.uint32_value(scope).unwrap_or(0) & 0xFFFF
}

// ── the shared bodies of Document / XPathEvaluator ──────────────────────────────────────────────────

fn create_expression<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments, iface: &str) -> Option<v8::Local<'s, v8::Object>> {
    let src = args.get(0).to_rust_string_lossy(scope);
    let (_, prefixes) = compile(scope, &src, args.get(1), iface, "createExpression")?;
    let obj = crate::install::live::mint_table(scope, EXPRESSION);
    brand(scope, obj, EXPRESSION);
    let s = str_val(scope, &src);
    evt_slot_write(scope, obj, SLOT_SOURCE, s);
    let arr = v8::Array::new(scope, (prefixes.len() * 2) as i32);
    for (i, (p, u)) in prefixes.iter().enumerate() {
        let pv = str_val(scope, p);
        arr.set_index(scope, (i * 2) as u32, pv);
        let uv = str_val(scope, u);
        arr.set_index(scope, (i * 2 + 1) as u32, uv);
    }
    evt_slot_write(scope, obj, SLOT_PREFIXES, arr.into());
    Some(obj)
}

fn evaluate_call(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, rv: &mut v8::ReturnValue, iface: &str) {
    let src = args.get(0).to_rust_string_lossy(scope);
    let Some(ctx) = context_node(scope, args.get(1), iface, "evaluate", 2) else { return };
    let Some((expr, _)) = compile(scope, &src, args.get(2), iface, "evaluate") else { return };
    let t = type_arg(scope, args.get(3));
    if let Some(r) = evaluate_to_result(scope, &expr, ctx, t, iface, "evaluate") {
        rv.set(r.into());
    }
}

pub(crate) fn cb_doc_create_expression(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if let Some(o) = create_expression(scope, &args, "Document") {
        rv.set(o.into());
    }
}

pub(crate) fn cb_doc_evaluate(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    evaluate_call(scope, &args, &mut rv, "Document");
}

/// `createNSResolver(nodeResolver)` hands the node back (Chrome 150: `createNSResolver(body) === body`).
pub(crate) fn cb_create_ns_resolver(_scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    rv.set(args.get(0));
}

fn cb_evaluator_create_expression(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if let Some(o) = create_expression(scope, &args, EVALUATOR) {
        rv.set(o.into());
    }
}

fn cb_evaluator_evaluate(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    evaluate_call(scope, &args, &mut rv, EVALUATOR);
}

/// `new XPathEvaluator()`: a branded instance on the table's prototype (`XPathEvaluator()` without
/// `new` is Chrome's "Please use the 'new' operator" TypeError).
pub(crate) fn cb_evaluator_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if args.new_target().is_undefined() {
        let msg = crate::binding::protos::use_new_msg(EVALUATOR);
        throw_type_error(scope, &msg);
        return;
    }
    brand(scope, args.this(), EVALUATOR);
}

// ── XPathExpression ─────────────────────────────────────────────────────────────────────────────────

fn cb_expression_evaluate(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let Some(ctx) = context_node(scope, args.get(0), EXPRESSION, "evaluate", 1) else { return };
    let src = evt_slot_read(scope, this, SLOT_SOURCE).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    let mut prefixes: Vec<(String, String)> = Vec::new();
    if let Some(arr) = evt_slot_read(scope, this, SLOT_PREFIXES).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) {
        let n = arr.length();
        let mut i = 0;
        while i + 1 < n {
            let p = arr.get_index(scope, i).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
            let u = arr.get_index(scope, i + 1).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
            prefixes.push((p, u));
            i += 2;
        }
    }
    // The source already compiled once with these prefixes; re-parsing it cannot fail.
    let Ok(expr) = parse(&src, &mut |p: &str| Ok(prefixes.iter().find(|(q, _)| q == p).map(|(_, u)| u.clone()))) else { return };
    let t = type_arg(scope, args.get(1));
    if let Some(r) = evaluate_to_result(scope, &expr, ctx, t, EXPRESSION, "evaluate") {
        rv.set(r.into());
    }
}

// ── XPathResult ─────────────────────────────────────────────────────────────────────────────────────

fn result_type(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> u32 {
    evt_slot_read(scope, this, SLOT_TYPE).and_then(|v| v.uint32_value(scope)).unwrap_or(0)
}

fn result_nodes<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<v8::Object>) -> Option<v8::Local<'s, v8::Array>> {
    evt_slot_read(scope, this, SLOT_NODES).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok())
}

fn invalid_iterator(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> bool {
    let Some(rev) = evt_slot_read(scope, this, SLOT_REVISION).and_then(|v| v.number_value(scope)) else { return false };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let now = op_state.borrow().borrow::<HostDom>().0.revision();
    now as f64 != rev
}

fn read_error(scope: &mut v8::PinScope, member: &str, what: &str) {
    throw_type_error(scope, &format!("Failed to read the '{member}' property from 'XPathResult': The result type is not {what}."));
}

fn cb_result_type(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let t = result_type(scope, args.this());
    rv.set(v8::Integer::new_from_unsigned(scope, t).into());
}

fn cb_number_value(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if result_type(scope, args.this()) != NUMBER_TYPE {
        return read_error(scope, "numberValue", "a number");
    }
    if let Some(v) = evt_slot_read(scope, args.this(), SLOT_VALUE) {
        rv.set(v);
    }
}

fn cb_string_value(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if result_type(scope, args.this()) != STRING_TYPE {
        return read_error(scope, "stringValue", "a string");
    }
    if let Some(v) = evt_slot_read(scope, args.this(), SLOT_VALUE) {
        rv.set(v);
    }
}

fn cb_boolean_value(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if result_type(scope, args.this()) != BOOLEAN_TYPE {
        return read_error(scope, "booleanValue", "a boolean");
    }
    if let Some(v) = evt_slot_read(scope, args.this(), SLOT_VALUE) {
        rv.set(v);
    }
}

fn cb_single_node_value(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !matches!(result_type(scope, args.this()), ANY_UNORDERED_NODE_TYPE | FIRST_ORDERED_NODE_TYPE) {
        return read_error(scope, "singleNodeValue", "a single node");
    }
    match result_nodes(scope, args.this()).and_then(|a| if a.length() > 0 { a.get_index(scope, 0) } else { None }) {
        Some(v) => rv.set(v),
        None => rv.set_null(),
    }
}

fn cb_invalid_iterator_state(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let is_iter = matches!(result_type(scope, args.this()), UNORDERED_NODE_ITERATOR_TYPE | ORDERED_NODE_ITERATOR_TYPE);
    let b = is_iter && invalid_iterator(scope, args.this());
    rv.set(v8::Boolean::new(scope, b).into());
}

fn cb_snapshot_length(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !matches!(result_type(scope, args.this()), UNORDERED_NODE_SNAPSHOT_TYPE | ORDERED_NODE_SNAPSHOT_TYPE) {
        return read_error(scope, "snapshotLength", "a snapshot");
    }
    let n = result_nodes(scope, args.this()).map(|a| a.length()).unwrap_or(0);
    rv.set(v8::Integer::new_from_unsigned(scope, n).into());
}

fn cb_iterate_next(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    if !matches!(result_type(scope, this), UNORDERED_NODE_ITERATOR_TYPE | ORDERED_NODE_ITERATOR_TYPE) {
        throw_type_error(scope, "Failed to execute 'iterateNext' on 'XPathResult': The result type is not an iterator.");
        return;
    }
    if invalid_iterator(scope, this) {
        dom_throw(scope, "Failed to execute 'iterateNext' on 'XPathResult': The document has mutated since the result was returned.", "InvalidStateError");
        return;
    }
    let i = evt_slot_read(scope, this, SLOT_INDEX).and_then(|v| v.uint32_value(scope)).unwrap_or(0);
    let Some(arr) = result_nodes(scope, this) else { return rv.set_null() };
    if i >= arr.length() {
        return rv.set_null();
    }
    let next: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, i + 1).into();
    evt_slot_write(scope, this, SLOT_INDEX, next);
    match arr.get_index(scope, i) {
        Some(v) => rv.set(v),
        None => rv.set_null(),
    }
}

fn cb_snapshot_item(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    if !matches!(result_type(scope, this), UNORDERED_NODE_SNAPSHOT_TYPE | ORDERED_NODE_SNAPSHOT_TYPE) {
        throw_type_error(scope, "Failed to execute 'snapshotItem' on 'XPathResult': The result type is not a snapshot.");
        return;
    }
    let idx = args.get(0).uint32_value(scope).unwrap_or(0);
    match result_nodes(scope, this).and_then(|a| if idx < a.length() { a.get_index(scope, idx) } else { None }) {
        Some(v) => rv.set(v),
        None => rv.set_null(),
    }
}

// ── the tables ──────────────────────────────────────────────────────────────────────────────────────

pub fn install_xpath_result(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "resultType" => branded_getter_row(scope, proto, member, cb_result_type),
        "numberValue" => branded_getter_row(scope, proto, member, cb_number_value),
        "stringValue" => branded_getter_row(scope, proto, member, cb_string_value),
        "booleanValue" => branded_getter_row(scope, proto, member, cb_boolean_value),
        "singleNodeValue" => branded_getter_row(scope, proto, member, cb_single_node_value),
        "invalidIteratorState" => branded_getter_row(scope, proto, member, cb_invalid_iterator_state),
        "snapshotLength" => branded_getter_row(scope, proto, member, cb_snapshot_length),
        "iterateNext" => branded_method_row(scope, proto, member, cb_iterate_next),
        "snapshotItem" => branded_method_row(scope, proto, member, cb_snapshot_item),
        other => debug_assert!(false, "install_xpath_result: unexpected member {other}"),
    }
}

pub fn install_xpath_expression(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "evaluate" => branded_method_row(scope, proto, member, cb_expression_evaluate),
        other => debug_assert!(false, "install_xpath_expression: unexpected member {other}"),
    }
}

pub fn install_xpath_evaluator(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "createExpression" => branded_method_row(scope, proto, member, cb_evaluator_create_expression),
        "createNSResolver" => branded_method_row(scope, proto, member, cb_create_ns_resolver),
        "evaluate" => branded_method_row(scope, proto, member, cb_evaluator_evaluate),
        other => debug_assert!(false, "install_xpath_evaluator: unexpected member {other}"),
    }
}

// Rows VERBATIM from the three blocks of `install/generated_registry.rs` they replace, in the
// capture's order. `[.., window]`: neither worker capture lists any of the three.
webapi::interface! {
    "XPathResult" [interface, window] {
        ctor     "XPathResult"                                         len 0 = stub,
        attr     "resultType"           : Integer   readonly                 = install_xpath_result,
        attr     "numberValue"          : Double    readonly                 = install_xpath_result,
        attr     "stringValue"          : DomString readonly                 = install_xpath_result,
        attr     "booleanValue"         : Boolean   readonly                 = install_xpath_result,
        attr     "singleNodeValue"      : Any       readonly nullable        = install_xpath_result,
        attr     "invalidIteratorState" : Boolean   readonly                 = install_xpath_result,
        attr     "snapshotLength"       : Integer   readonly                 = install_xpath_result,
        constant "ANY_TYPE"                      : Any = 0,
        constant "NUMBER_TYPE"                   : Any = 1,
        constant "STRING_TYPE"                   : Any = 2,
        constant "BOOLEAN_TYPE"                  : Any = 3,
        constant "UNORDERED_NODE_ITERATOR_TYPE"  : Any = 4,
        constant "ORDERED_NODE_ITERATOR_TYPE"    : Any = 5,
        constant "UNORDERED_NODE_SNAPSHOT_TYPE"  : Any = 6,
        constant "ORDERED_NODE_SNAPSHOT_TYPE"    : Any = 7,
        constant "ANY_UNORDERED_NODE_TYPE"       : Any = 8,
        constant "FIRST_ORDERED_NODE_TYPE"       : Any = 9,
        method   "iterateNext"          : Any       len 0 nullable           = install_xpath_result,
        method   "snapshotItem"         : Any       len 1 required 1 nullable = install_xpath_result,
        static_constant "ANY_TYPE"                      : Any = 0,
        static_constant "NUMBER_TYPE"                   : Any = 1,
        static_constant "STRING_TYPE"                   : Any = 2,
        static_constant "BOOLEAN_TYPE"                  : Any = 3,
        static_constant "UNORDERED_NODE_ITERATOR_TYPE"  : Any = 4,
        static_constant "ORDERED_NODE_ITERATOR_TYPE"    : Any = 5,
        static_constant "UNORDERED_NODE_SNAPSHOT_TYPE"  : Any = 6,
        static_constant "ORDERED_NODE_SNAPSHOT_TYPE"    : Any = 7,
        static_constant "ANY_UNORDERED_NODE_TYPE"       : Any = 8,
        static_constant "FIRST_ORDERED_NODE_TYPE"       : Any = 9
    }
}

webapi::interface! {
    "XPathExpression" [interface, window] {
        ctor   "XPathExpression"                        len 0 = stub,
        method "evaluate" : Any len 1 required 1              = install_xpath_expression
    }
}

webapi::interface! {
    "XPathEvaluator" [value, window] {
        ctor   "XPathEvaluator"                                len 0 = stub,
        method "createExpression" : Any len 1 required 1             = install_xpath_evaluator,
        method "createNSResolver" : Any len 1 required 1             = install_xpath_evaluator,
        method "evaluate"         : Any len 2 required 2             = install_xpath_evaluator
    }
}

/// The registry walk's creators (`install::live::ORDERED_HAND`), one per interface, at each name's
/// capture position -- `XPathResult`, `XPathExpression`, `XPathEvaluator` sit together near the head
/// of every window capture.
pub fn create_xpath_result<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, ledger: &mut crate::install::installer::InstallLedger) {
    use crate::install::installer::{self, Flavor};
    installer::create_interface(scope, global, &XPATHRESULT, "XPathResult", None, None, Flavor::Table, ledger);
}

pub fn create_xpath_expression<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, ledger: &mut crate::install::installer::InstallLedger) {
    use crate::install::installer::{self, Flavor};
    installer::create_interface(scope, global, &XPATHEXPRESSION, "XPathExpression", None, None, Flavor::Table, ledger);
}

pub fn create_xpath_evaluator<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, ledger: &mut crate::install::installer::InstallLedger) {
    crate::install::live::rows::build_newable(scope, global, &XPATHEVALUATOR, cb_evaluator_ctor, None, None, ledger);
}

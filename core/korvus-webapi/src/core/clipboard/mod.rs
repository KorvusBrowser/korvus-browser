//! `clipboard` Web-API interfaces. The DataTransfer family (DataTransfer/DataTransferItemList/
//! DataTransferItem) is tightly coupled and moved together into one file; splittable later.

pub(crate) mod data_transfer;
/// `ClipboardItem` as one `interface!` table, and the `supports()` mandatory/`web `-prefix rule.
pub(crate) mod clipboard_item;

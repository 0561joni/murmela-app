use anyhow::{anyhow, Result};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardWriting};
use objc2_foundation::NSArray;

/// An eager copy of every readable representation currently present on the
/// macOS general pasteboard. Keeping the item boundaries intact preserves
/// rich text, images, file URLs, and application-specific clipboard formats.
pub(super) struct ClipboardSnapshot {
    items: Vec<Retained<NSPasteboardItem>>,
}

impl ClipboardSnapshot {
    pub(super) fn capture() -> Self {
        Self::capture_from(&NSPasteboard::generalPasteboard())
    }

    fn capture_from(pasteboard: &NSPasteboard) -> Self {
        if let Some(source_items) = pasteboard.pasteboardItems() {
            return Self::capture_items(&source_items);
        }

        Self { items: Vec::new() }
    }

    fn capture_items(source_items: &NSArray<NSPasteboardItem>) -> Self {
        let items = source_items
            .iter()
            .map(|source_item| {
                let captured_item = NSPasteboardItem::new();

                // Materialize every representation before Murmela replaces
                // the pasteboard. Some source apps provide their data lazily.
                for pasteboard_type in source_item.types().iter() {
                    if let Some(data) = source_item.dataForType(&pasteboard_type) {
                        let _ = captured_item.setData_forType(&data, &pasteboard_type);
                    }
                }

                captured_item
            })
            .collect();

        Self { items }
    }

    #[cfg(test)]
    fn items(&self) -> &[Retained<NSPasteboardItem>] {
        &self.items
    }

    pub(super) fn restore_if_unchanged(self, expected_change_count: isize) -> Result<bool> {
        let pasteboard = NSPasteboard::generalPasteboard();
        if !should_restore(expected_change_count, pasteboard.changeCount()) {
            return Ok(false);
        }

        self.restore_to(&pasteboard)?;
        Ok(true)
    }

    fn restore_to(self, pasteboard: &NSPasteboard) -> Result<()> {
        pasteboard.clearContents();
        if self.items.is_empty() {
            return Ok(());
        }

        let writable_items: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = self
            .items
            .into_iter()
            .map(ProtocolObject::from_retained)
            .collect();
        let objects = NSArray::from_retained_slice(&writable_items);

        if pasteboard.writeObjects(&objects) {
            Ok(())
        } else {
            Err(anyhow!("macOS rejected the saved clipboard contents"))
        }
    }
}

pub(super) fn change_count() -> isize {
    NSPasteboard::generalPasteboard().changeCount()
}

fn should_restore(expected_change_count: isize, current_change_count: isize) -> bool {
    expected_change_count == current_change_count
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_foundation::{NSData, NSString};

    #[test]
    fn restores_only_when_murmela_still_owns_the_latest_clipboard_change() {
        assert!(should_restore(42, 42));
        assert!(!should_restore(42, 43));
    }

    #[test]
    fn snapshot_preserves_items_and_all_their_representations() {
        let text_type = NSString::from_str("public.utf8-plain-text");
        let rich_type = NSString::from_str("public.rtf");
        let file_type = NSString::from_str("public.file-url");

        let first_item = NSPasteboardItem::new();
        assert!(first_item.setData_forType(&NSData::with_bytes(b"plain"), &text_type));
        assert!(first_item.setData_forType(&NSData::with_bytes(b"rich"), &rich_type));

        let second_item = NSPasteboardItem::new();
        let file_data = NSData::with_bytes(b"file:///tmp/example.txt");
        assert!(second_item.setData_forType(&file_data, &file_type));

        let original_items = NSArray::from_retained_slice(&[first_item, second_item]);
        let snapshot = ClipboardSnapshot::capture_items(&original_items);
        let restored_items = snapshot.items();
        assert_eq!(restored_items.len(), 2);
        assert_eq!(
            restored_items[0]
                .dataForType(&text_type)
                .expect("plain-text representation")
                .to_vec(),
            b"plain",
        );
        assert_eq!(
            restored_items[0]
                .dataForType(&rich_type)
                .expect("rich-text representation")
                .to_vec(),
            b"rich",
        );
        assert_eq!(
            restored_items[1]
                .dataForType(&file_type)
                .expect("file representation")
                .to_vec(),
            b"file:///tmp/example.txt",
        );
    }
}

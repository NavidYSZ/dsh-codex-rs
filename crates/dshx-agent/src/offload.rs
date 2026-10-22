//! Image offload executor (dsh `compaction-image-offload`): when a route
//! rejects a request with `IMAGE_OFFLOAD_REQUIRED`, one `image/offload` event
//! permanently replaces the oldest retained input images with placeholder
//! text, and the request is rebuilt without spending the retry budget.
use dshx_core::{ContentBlock, LlmFailure, NewEvent, kinds};
use serde_json::json;

use crate::events::event;
use crate::journal::Journal;

/// The count of oldest images an `IMAGE_OFFLOAD_REQUIRED` failure asks for.
pub(crate) fn required(failure: &LlmFailure) -> Option<u64> {
    if failure.code == "IMAGE_OFFLOAD_REQUIRED" {
        failure.offload_images
    } else {
        None
    }
}

/// The decision offloading `count` more retained image occurrences among
/// `seqs` (input messages in request order), or `None` when none remain.
/// Assistant nodes carry model output and are skipped (dsh `offloadOldestImages`).
pub(crate) fn select(journal: &Journal, seqs: &[u64], mut count: u64) -> Option<NewEvent> {
    let targets: Vec<_> = journal.read(|events, surface| {
        let mut targets = Vec::new();
        for seq in seqs {
            if count == 0 {
                break;
            }
            let Some(e) = events.get(*seq as usize) else {
                continue;
            };
            if e.kind != kinds::USER_MESSAGE && e.kind != kinds::TOOL_RESULT {
                continue;
            }
            let Some(message) = surface.message(e) else {
                continue;
            };
            let mut indexes = Vec::new();
            let images = message.content.iter().filter_map(|b| match b {
                ContentBlock::Image { offloaded, .. } => Some(*offloaded),
                _ => None,
            });
            for (index, offloaded) in images.enumerate() {
                if count == 0 {
                    break;
                }
                if !offloaded {
                    indexes.push(index);
                    count -= 1;
                }
            }
            if !indexes.is_empty() {
                targets.push(json!({"seq": seq, "imageIndexes": indexes}));
            }
        }
        targets
    });
    (!targets.is_empty()).then(|| event(kinds::IMAGE_OFFLOAD, json!({ "targets": targets })))
}

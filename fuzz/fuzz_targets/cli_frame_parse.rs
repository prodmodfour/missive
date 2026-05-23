#![no_main]

use std::io::Cursor;

use libfuzzer_sys::fuzz_target;
use missive_adapters::{
    FileDropInputFile, HttpInputFrame, StdioInputFrame, read_ndjson_frames, read_single_frame,
};
use serde_json::Value;

const MAX_INPUT_BYTES: usize = 64 * 1024;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }

    let input = std::str::from_utf8(data).unwrap_or_default();
    let _ = StdioInputFrame::from_json_str(input);
    let _ = HttpInputFrame::from_json_str(input);
    let _ = FileDropInputFile::from_json_str(input);

    let mut single_frame_reader = Cursor::new(data);
    let _ = read_single_frame(&mut single_frame_reader);

    let mut ndjson_reader = Cursor::new(data);
    let _ = read_ndjson_frames(&mut ndjson_reader, |_sequence, _frame_result| Ok(()));

    if let Ok(value) = serde_json::from_slice::<Value>(data) {
        let _ = StdioInputFrame::from_value(value.clone());
        let _ = HttpInputFrame::from_value(value.clone());
        let _ = FileDropInputFile::from_value(value);
    }
});

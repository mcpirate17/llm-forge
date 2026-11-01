use anyhow::Result;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::Path;

pub fn memory_fixture(root: &Path) -> Result<u64> {
    let fingerprint = format!("sha256:{}", "b".repeat(64));
    let vector = vec![0.125_f64; 64];
    let query = json!({"schema_version":1,"task_sha256":format!("{:x}",Sha256::digest(b"native tools")),
        "embedding_fingerprint":fingerprint,"vector":vector});
    fs::write(root.join("query.json"), serde_json::to_vec(&query)?)?;
    let path = root.join("memory.jsonl");
    let mut output = BufWriter::new(File::create(&path)?);
    for index in 0..4096 {
        let row = json!({"schema_version":3,"embedding":{"fingerprint":fingerprint,"dimension":64,"paid":true},
            "source_sha256":format!("{index:064x}"),"source":"notes","path":format!("notes/{index}.md"),
            "title":format!("note {index}"),"text":"Native tooling benchmark fixture", "vector":vector});
        serde_json::to_writer(&mut output, &row)?;
        output.write_all(b"\n")?;
    }
    output.flush()?;
    Ok(fs::metadata(path)?.len())
}

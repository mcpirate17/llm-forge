#![allow(dead_code)]
use forge::tokenizer::Config;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokenizers::{
    models::bpe::BPE, pre_tokenizers::byte_level::ByteLevel,
    processors::template::TemplateProcessing, AddedToken, Tokenizer,
};

pub struct Fixture {
    pub root: PathBuf,
    pub config: Config,
    pub tokenizer: Tokenizer,
}
impl Fixture {
    pub fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "forge-tokenizer-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let chars = byte_chars();
        let mut vocab: tokenizers::models::bpe::Vocab = chars
            .iter()
            .enumerate()
            .map(|(i, c)| (c.to_string(), i as u32))
            .collect();
        let merges = merges();
        for (i, (a, b)) in merges.iter().enumerate() {
            vocab.insert(format!("{a}{b}"), 256 + i as u32);
        }
        let bos = vocab.len() as u32;
        vocab.insert("<bos>".into(), bos);
        vocab.insert("<eos>".into(), bos + 1);
        let bpe = BPE::builder()
            .vocab_and_merges(vocab, merges)
            .cache_capacity(0)
            .build()
            .unwrap();
        let mut tokenizer = Tokenizer::new(bpe);
        tokenizer.with_pre_tokenizer(Some(ByteLevel::new(false, false, false)));
        tokenizer.with_decoder(Some(ByteLevel::new(false, false, false)));
        tokenizer
            .add_special_tokens([
                AddedToken::from("<bos>", true),
                AddedToken::from("<eos>", true),
            ])
            .unwrap();
        tokenizer.with_post_processor(Some(
            TemplateProcessing::builder()
                .try_single("<bos> $A <eos>")
                .unwrap()
                .special_tokens(vec![("<bos>", bos), ("<eos>", bos + 1)])
                .build()
                .unwrap(),
        ));
        let bytes = tokenizer.to_string(false).unwrap();
        let path = root.join("tokenizer.json");
        fs::write(&path, bytes.as_bytes()).unwrap();
        let config = Config {
            path,
            tokenizer_sha256: Sha256::digest(bytes.as_bytes()).into(),
            model_sha256: [7; 32],
            add_special_tokens: false,
        };
        Self {
            root,
            config,
            tokenizer,
        }
    }
    pub fn update(&mut self, change: impl FnOnce(&mut serde_json::Value)) {
        let mut value = serde_json::from_slice(&fs::read(&self.config.path).unwrap()).unwrap();
        change(&mut value);
        let bytes = serde_json::to_vec(&value).unwrap();
        fs::write(&self.config.path, &bytes).unwrap();
        self.config.tokenizer_sha256 = Sha256::digest(&bytes).into();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

pub fn byte_chars() -> Vec<char> {
    let mut extra = 256;
    (0..256)
        .map(|b| {
            let kept =
                (33..=126).contains(&b) || (161..=172).contains(&b) || (174..=255).contains(&b);
            let cp = if kept {
                b
            } else {
                let n = extra;
                extra += 1;
                n
            };
            char::from_u32(cp).unwrap()
        })
        .collect()
}
pub fn merges() -> Vec<(String, String)> {
    [
        ("h", "e"),
        ("he", "l"),
        ("hel", "l"),
        ("hell", "o"),
        ("Ġ", "w"),
        ("Ġw", "o"),
        ("Ġwo", "r"),
        ("Ġwor", "l"),
        ("Ġworl", "d"),
        ("a", "a"),
        ("aa", "a"),
    ]
    .into_iter()
    .map(|(a, b)| (a.into(), b.into()))
    .collect()
}
/// Deliberately simple independent O(n²) byte-BPE control, without HF encoding.
pub fn reference(text: &str) -> Vec<u32> {
    let chars = byte_chars();
    let merges = merges();
    let mut parts: Vec<String> = text
        .bytes()
        .map(|b| chars[b as usize].to_string())
        .collect();
    loop {
        let best = parts
            .windows(2)
            .enumerate()
            .filter_map(|(i, p)| {
                merges
                    .iter()
                    .position(|(a, b)| a == &p[0] && b == &p[1])
                    .map(|rank| (rank, i))
            })
            .min();
        let Some((_, i)) = best else { break };
        let next = parts.remove(i + 1);
        parts[i].push_str(&next);
    }
    parts
        .iter()
        .map(|part| {
            if let Some(i) = chars.iter().position(|c| c.to_string() == *part) {
                i as u32
            } else {
                256 + merges
                    .iter()
                    .position(|(a, b)| format!("{a}{b}") == *part)
                    .unwrap() as u32
            }
        })
        .collect()
}

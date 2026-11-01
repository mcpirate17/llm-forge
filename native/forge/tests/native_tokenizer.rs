#[path = "support/tokenizer_fixture.rs"]
mod fixture;
use fixture::Fixture;
use forge::tokenizer::{Counter, MAX_TEXT_BYTES};
use std::fs;

#[test]
fn native_bpe_matches_independent_ranked_merge_control_and_roundtrips_unicode() {
    let fixture = Fixture::new();
    let counter = Counter::load(&fixture.config).unwrap();
    let long = "x".repeat(257);
    let inputs = [
        "",
        "hello world!",
        "aaaaaa aaaa aaa aa a",
        "é\u{301} 東京 🦀\n\t\0",
        "{\"key\":42}\n",
        &long,
    ];
    for text in inputs {
        let encoding = counter.encode(text).unwrap();
        assert_eq!(encoding.get_ids(), fixture::reference(text), "{text:?}");
        assert_eq!(counter.count(text).unwrap(), encoding.len());
        assert_eq!(counter.decode(encoding.get_ids()).unwrap(), text);
    }
    let counter = std::sync::Arc::new(counter);
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let counter = counter.clone();
            scope.spawn(move || {
                for _ in 0..100 {
                    assert_eq!(
                        counter.encode("hello world!").unwrap().get_ids(),
                        &[259, 264, 33]
                    );
                }
            });
        }
    });
}

#[test]
fn padding_truncation_special_tokens_and_recipe_identity_cannot_silently_change_counts() {
    let mut fixture = Fixture::new();
    fixture.update(|v| {
        v["truncation"] = serde_json::json!({"direction":"Right","max_length":1,"strategy":"LongestFirst","stride":0});
        v["padding"] = serde_json::json!({"strategy":{"Fixed":128},"direction":"Right","pad_to_multiple_of":null,"pad_id":0,"pad_type_id":0,"pad_token":"!"});
    });
    let plain = Counter::load(&fixture.config).unwrap();
    assert_eq!(plain.count("hello world!").unwrap(), 3);
    fixture.config.add_special_tokens = true;
    let special = Counter::load(&fixture.config).unwrap();
    assert_eq!(special.count("hello world!").unwrap(), 5);
    assert_eq!(special.count("").unwrap(), 2);
    assert_ne!(
        plain.identity().recipe_sha256,
        special.identity().recipe_sha256
    );
    fixture.config.model_sha256 = [8; 32];
    assert_ne!(
        special.identity().recipe_sha256,
        Counter::load(&fixture.config)
            .unwrap()
            .identity()
            .recipe_sha256
    );
}

#[test]
fn wrong_artifacts_stochastic_models_symlinks_and_oversized_inputs_fail_closed() {
    let mut fixture = Fixture::new();
    let counter = Counter::load(&fixture.config).unwrap();
    assert!(counter.count(&"a".repeat(MAX_TEXT_BYTES + 1)).is_err());
    assert!(counter.decode(&[u32::MAX]).is_err());
    fixture.config.tokenizer_sha256 = [9; 32];
    assert!(Counter::load(&fixture.config).is_err());
    fixture.update(|v| {
        v["model"]["dropout"] = 0.5.into();
    });
    assert!(Counter::load(&fixture.config).is_err());
    fixture.update(|v| {
        v["model"] =
            serde_json::json!({"type":"WordLevel","vocab":{"<unk>":0},"unk_token":"<unk>"});
    });
    assert!(Counter::load(&fixture.config).is_err());
    let link = fixture.root.join("link.json");
    std::os::unix::fs::symlink(&fixture.config.path, &link).unwrap();
    fixture.config.path = link;
    assert!(Counter::load(&fixture.config).is_err());
}

#[test]
fn tokenizer_cli_is_native_with_an_empty_path() {
    let fixture = Fixture::new();
    let config = fixture.root.join("config.json");
    let input = fixture.root.join("prompt.txt");
    fs::write(&config, serde_json::to_vec(&fixture.config).unwrap()).unwrap();
    fs::write(&input, "hello world!").unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_forge"))
        .env("PATH", "")
        .args(["context", "tokens", "--tokenizer"])
        .arg(config)
        .arg("--input")
        .arg(input)
        .arg("--ids")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["ids"], serde_json::json!([259, 264, 33]));
    assert_eq!(result["tokens"], 3);
}

#[test]
fn bounded_count_cache_handles_collision_eviction_without_cross_text_counts() {
    let fixture = Fixture::new();
    let counter = Counter::load(&fixture.config).unwrap();
    let inputs: Vec<String> = (0..600)
        .map(|i| format!("history-{i}: {} 東京", "hello world! ".repeat(i % 17)))
        .collect();
    for text in inputs.iter().chain(inputs.iter().rev()) {
        assert_eq!(counter.count(text).unwrap(), fixture::reference(text).len());
    }
}

#[test]
#[ignore = "requires the separately downloaded pinned GPT-2 tokenizer artifact; no network in tests"]
fn pinned_gpt2_artifact_matches_known_ids_and_roundtrips_unicode() {
    let path = std::env::var_os("FORGE_GPT2_TOKENIZER").expect("set FORGE_GPT2_TOKENIZER");
    let config = forge::tokenizer::Config {
        path: path.into(),
        tokenizer_sha256: conductor_native::sha256_hex::parse(
            "8414cab924d8b9b33013f0d221c5862f365ee9be39c5c2bfae8a5a9e970478a6",
        )
        .unwrap(),
        // This is a tokenizer-only fixture identity, not loaded model weights.
        model_sha256: [7; 32],
        add_special_tokens: false,
    };
    let counter = Counter::load(&config).unwrap();
    for (text, ids) in [
        ("hello world!", vec![31373, 995, 0]),
        ("Hello, world!", vec![15496, 11, 995, 0]),
        ("2+2=4", vec![17, 10, 17, 28, 19]),
        ("<|endoftext|>", vec![50256]),
    ] {
        assert_eq!(counter.encode(text).unwrap().get_ids(), ids);
        assert_eq!(counter.count(text).unwrap(), ids.len());
    }
    for text in [
        "東京🦀 é\u{301}\n\0",
        "fn main() { println!(\"hello\"); }",
        "\t \r\n ",
        "",
    ] {
        let encoding = counter.encode(text).unwrap();
        assert_eq!(counter.count(text).unwrap(), encoding.len());
        assert_eq!(counter.decode(encoding.get_ids()).unwrap(), text);
    }
}

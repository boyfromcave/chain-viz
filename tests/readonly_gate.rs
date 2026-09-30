//! Plan §4.2: chain-viz never calls a writer. This gate greps `src/` for every forbidden RPC
//! name; a match anywhere in the source (code, string, comment) fails the build. The recording
//! script that made `tests/fixtures/*.json` used `generate`/`invalidateblock` on ONE devnet node
//! and lives outside this repository on purpose.

use std::fs;
use std::path::Path;

const FORBIDDEN: &[&str] = &[
    "yed_setquote",
    "yed_addattestation",
    "yed_mint",
    "yed_send",
    "yed_sendmany",
    "yed_redeem",
    "yed_claim",
    "yed_sweep",
    "yed_claimnotice",
    "yed_sweepcarriers",
    "yed_registerattestor",
    "yed_withdrawbond",
    "yed_revive",
    "yed_reportequivocation",
    "yed_signattestation",
    "yed_getnewaddress",
    "yed_lockcoins",
    "yed_unlockcoin",
    "yed_buildbundle",
    "generate",
    "generatetoaddress",
    "submitblock",
    "sendrawtransaction",
    "getblocktemplate",
    "invalidateblock",
    "reconsiderblock",
    "sendtoaddress",
    "sendmany",
    "z_sendmany",
    "importprivkey",
    "dumpprivkey",
    "walletpassphrase",
    "getnewaddress",
    "signrawtransaction",
    "createrawtransaction",
];

fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(dir).expect("read src/") {
        let p = entry.expect("entry").path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().map(|e| e == "rs").unwrap_or(false) {
            out.push(p);
        }
    }
}

/// A name counts only as a whole RPC identifier (`generate` must not match `generated`).
fn mentions(text: &str, name: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(i) = text[from..].find(name) {
        let start = from + i;
        let end = start + name.len();
        let before = start == 0 || !(bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_');
        let after = end == bytes.len() || !(bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_');
        if before && after {
            return Some(text[..start].matches('\n').count() + 1);
        }
        from = end;
    }
    None
}

#[test]
fn src_never_names_a_writer() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    walk(&root, &mut files);
    assert!(!files.is_empty());
    let mut hits = Vec::new();
    for f in &files {
        let text = fs::read_to_string(f).expect("read");
        for name in FORBIDDEN {
            if let Some(line) = mentions(&text, name) {
                hits.push(format!("{}:{}: {}", f.display(), line, name));
            }
        }
    }
    assert!(hits.is_empty(), "forbidden RPC names in src/ (plan §4.2):\n{}", hits.join("\n"));
}

#[test]
fn whole_word_only() {
    assert_eq!(mentions("let generated = 1;", "generate"), None);
    assert_eq!(mentions("call(\"generate\")", "generate"), Some(1));
    assert_eq!(mentions("a\nb yed_mint(", "yed_mint"), Some(2));
    assert_eq!(mentions("yed_mintable", "yed_mint"), None);
}

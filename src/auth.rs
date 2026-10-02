// Copyright (c) 2026 The Ycash developers
// Distributed under the MIT software license, see the accompanying
// file LICENSE or https://www.opensource.org/licenses/mit-license.php .

//! Node credentials from a Bitcoin-style cookie file: `ycashd` without `-rpcuser`/`-rpcpassword`
//! writes `<datadir>/.cookie` (`<datadir>/regtest/.cookie` on regtest, `testnet3/.cookie` on
//! testnet) holding `__cookie__:<token>`, readable by the node's owner only. `--cookie <path>`
//! names the file; `--datadir <dir>` looks in the usual places. The pair applies to every
//! `--nodes` URL that carries no userinfo, ahead of `--rpcuser`/`--rpcpassword`.

use std::path::{Path, PathBuf};

/// Where `ycashd` puts the cookie under a datadir, in the order tried.
pub fn cookie_candidates(datadir: &Path) -> Vec<PathBuf> {
    vec![datadir.join(".cookie"), datadir.join("regtest").join(".cookie"), datadir.join("testnet3").join(".cookie")]
}

/// Parse a cookie file's text: `user:password`, one line, trailing whitespace ignored.
pub fn parse_cookie(text: &str) -> Result<(String, String), String> {
    let line = text.lines().next().unwrap_or("").trim();
    let (user, password) = line.split_once(':').ok_or("cookie file: expected user:password")?;
    if password.is_empty() {
        return Err("cookie file: empty password".into());
    }
    Ok((user.to_string(), password.to_string()))
}

/// Read the cookie at `path`.
pub fn read_cookie(path: &Path) -> Result<(String, String), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cookie {}: {}", path.display(), e))?;
    parse_cookie(&text).map_err(|e| format!("{} ({})", e, path.display()))
}

/// Read the cookie under a datadir, trying the network subdirectories.
pub fn read_cookie_in(datadir: &Path) -> Result<(String, String), String> {
    let candidates = cookie_candidates(datadir);
    for c in &candidates {
        if c.is_file() {
            return read_cookie(c);
        }
    }
    Err(format!("no .cookie under {} (looked in {}); is the node running without -rpcuser?", datadir.display(), candidates.iter().map(|c| c.display().to_string()).collect::<Vec<_>>().join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        assert_eq!(parse_cookie("__cookie__:abc123\n").unwrap(), ("__cookie__".into(), "abc123".into()));
        assert!(parse_cookie("nocolon").is_err());
        assert!(parse_cookie("u:").is_err());
    }

    #[test]
    fn finds_regtest_subdir() {
        let dir = std::env::temp_dir().join(format!("chain-viz-cookie-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("regtest")).unwrap();
        std::fs::write(dir.join("regtest").join(".cookie"), "__cookie__:tok").unwrap();
        assert_eq!(read_cookie_in(&dir).unwrap().1, "tok");
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(read_cookie_in(&dir).is_err());
    }
}

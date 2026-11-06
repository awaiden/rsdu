use regex::Regex;
use std::collections::HashSet;

#[derive(Default, Clone)]
pub struct Ignore {
    pub dirs: HashSet<String>,
    pub pattern: Option<Regex>,
    pub no_hidden: bool,
}

impl Ignore {
    /// `dirs` are exact paths, `patterns` are regexes matched against the whole path.
    pub fn new(
        dirs: &[String],
        patterns: &[String],
        no_hidden: bool,
    ) -> Result<Self, regex::Error> {
        let pattern = if patterns.is_empty() {
            None
        } else {
            let alt: Vec<String> = patterns.iter().map(|p| format!("(?:{p})")).collect();
            Some(Regex::new(&format!("^(?:{})$", alt.join("|")))?)
        };
        Ok(Self {
            dirs: dirs
                .iter()
                .map(|d| d.trim_end_matches('/').to_string())
                .collect(),
            pattern,
            no_hidden,
        })
    }

    #[inline]
    pub fn has_path_rules(&self) -> bool {
        !self.dirs.is_empty() || self.pattern.is_some()
    }

    #[inline]
    pub fn skip_path(&self, path: &str) -> bool {
        self.dirs.contains(path) || self.pattern.as_ref().is_some_and(|r| r.is_match(path))
    }

    #[cfg(test)]
    pub fn skip(&self, name: &str, path: &str) -> bool {
        (self.no_hidden && name.starts_with('.')) || self.skip_path(path)
    }

    /// Return a copy with `dirs` pruned to paths strictly inside `root`.
    pub fn scoped_to(&self, root: &str) -> Self {
        let root = root.trim_end_matches('/');
        let dirs = self
            .dirs
            .iter()
            .filter(|d| {
                root.is_empty()
                    || (d.starts_with(root) && d.as_bytes().get(root.len()) == Some(&b'/'))
            })
            .cloned()
            .collect();
        Self {
            dirs,
            pattern: self.pattern.clone(),
            no_hidden: self.no_hidden,
        }
    }
}

/// Read one pattern per line (blank lines skipped) for `-X`.
pub fn read_patterns(file: &str) -> std::io::Result<Vec<String>> {
    Ok(std::fs::read_to_string(file)?
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches() {
        let ig = Ignore::new(&["/proc".into()], &["/tmp/x.*".into()], true).unwrap();
        assert!(ig.skip("proc", "/proc"));
        assert!(ig.skip("xy", "/tmp/xy"));
        assert!(ig.skip(".git", "/a/.git"));
        assert!(!ig.skip("src", "/a/src"));
        assert!(!ig.scoped_to("/usr").dirs.contains("/proc"));
        assert!(ig.scoped_to("/").dirs.contains("/proc"));
    }
}

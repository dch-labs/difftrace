//! The flow map builder: a region's defines, callers, callees, and trigger sites, read mechanically from the pinned head.

use crate::contracts::{FlowMap, Region, SearchHit};
use crate::error::DtError;
use crate::gateway::PrGateway;

const TRIGGER_MARKERS: [&str; 7] = [
    ".spawn(",
    "::spawn(",
    ".await",
    ".register(",
    ".subscribe(",
    ".connect(",
    ".on(",
];

const CALL_KEYWORDS: [&str; 12] = [
    "if", "while", "for", "in", "loop", "match", "fn", "unsafe", "return", "else", "as", "dyn",
];

pub fn build(
    region: &Region,
    region_text: &str,
    gateway: &dyn PrGateway,
    max_callers: usize,
) -> Result<FlowMap, DtError> {
    let defines = scan_defines(region_text);
    let callee_names = scan_callees(region_text, &defines);
    if defines.is_empty() {
        return Ok(FlowMap {
            defines: Vec::new(),
            callers: Vec::new(),
            callees: callee_names,
            triggers: Vec::new(),
        });
    }
    let mut caller_sites: Vec<SearchHit> = Vec::new();
    let mut triggers: Vec<SearchHit> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    for (name, _) in &defines {
        if !names.contains(name) {
            names.push(name.clone());
        }
    }
    for name in &names {
        let def_slots = defines
            .iter()
            .filter(|(existing, _)| existing == name)
            .count();
        let search_cap = max_callers.saturating_add(def_slots);
        for hit in gateway
            .search_at_head(&format!("fn {name}("), search_cap)?
            .into_iter()
            .filter(|hit| !inside_region(hit, region))
        {
            if !triggers.contains(&hit) {
                triggers.push(hit);
            }
        }
        for hit in gateway.search_at_head(&format!("{name}("), search_cap)? {
            let own_definition = hit.file == region.file
                && defines
                    .iter()
                    .any(|(existing, line)| existing == name && hit.line == *line);
            if own_definition || triggers.contains(&hit) {
                continue;
            }
            if TRIGGER_MARKERS
                .iter()
                .any(|marker| hit.text.contains(marker))
            {
                triggers.push(hit);
            } else {
                caller_sites.push(hit);
            }
        }
    }
    Ok(FlowMap {
        defines: names,
        callers: cap_sorted(caller_sites, max_callers),
        callees: callee_names,
        triggers: cap_sorted(triggers, max_callers),
    })
}

fn scan_defines(region_text: &str) -> Vec<(String, u32)> {
    let mut defines: Vec<(String, u32)> = Vec::new();
    let mut line_no = 1;
    for line in stripped_lines(region_text) {
        if let Some(name) = define_name(&line) {
            defines.push((name, line_no));
        }
        line_no = line_no.saturating_add(1);
    }
    defines
}

fn define_name(line: &str) -> Option<String> {
    let mut words = line.split_whitespace();
    while let Some(word) = words.next() {
        if word != "fn" {
            continue;
        }
        let candidate = words.next()?;
        let name: String = candidate
            .chars()
            .take_while(|ch| ch.is_alphanumeric() || *ch == '_')
            .collect();
        if name.is_empty() {
            return None;
        }
        return Some(name);
    }
    None
}

fn scan_callees(region_text: &str, defines: &[(String, u32)]) -> Vec<String> {
    let mut callees: Vec<String> = Vec::new();
    for line in stripped_lines(region_text) {
        for token in call_tokens(&line) {
            let is_define = defines.iter().any(|(name, _)| *name == token);
            if is_define || CALL_KEYWORDS.contains(&token.as_str()) || callees.contains(&token) {
                continue;
            }
            callees.push(token);
        }
    }
    callees
}

fn call_tokens(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in line.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            current.push(ch);
        } else {
            if ch == '(' && !current.is_empty() {
                tokens.push(current.clone());
            }
            current.clear();
        }
    }
    tokens
}

pub(crate) fn stripped_lines(region_text: &str) -> Vec<String> {
    let mut stripper = LineStripper::default();
    region_text
        .lines()
        .map(|line| stripper.strip(line))
        .collect()
}

#[derive(Debug, Clone, Copy)]
enum StripState {
    Normal,
    PendingSlash,
    LineComment,
    Block {
        depth: usize,
        saw_star: bool,
        saw_slash: bool,
    },
    InString {
        escaped: bool,
    },
    SawQuote,
    CharOrLifetime {
        buffered: char,
    },
    InChar {
        escaped: bool,
    },
    SawR,
    RawHashes {
        count: usize,
    },
    InRaw {
        needed: usize,
    },
    RawClose {
        needed: usize,
        matched: usize,
    },
}

struct LineStripper {
    state: StripState,
}

impl Default for LineStripper {
    fn default() -> Self {
        Self {
            state: StripState::Normal,
        }
    }
}

impl LineStripper {
    fn strip(&mut self, line: &str) -> String {
        let mut out = String::new();
        for ch in line.chars() {
            self.step(ch, &mut out);
        }
        self.finish_line(&mut out);
        out
    }

    fn step(&mut self, ch: char, out: &mut String) {
        self.state = match self.state {
            StripState::Normal => step_normal(ch, out),
            StripState::PendingSlash => step_pending_slash(ch, out),
            StripState::LineComment => StripState::LineComment,
            StripState::Block {
                depth,
                saw_star,
                saw_slash,
            } => step_block(depth, saw_star, saw_slash, ch),
            StripState::InString { escaped } => step_string(escaped, ch),
            StripState::SawQuote => step_saw_quote(ch),
            StripState::CharOrLifetime { buffered } => step_char_or_lifetime(buffered, ch, out),
            StripState::InChar { escaped } => step_char(escaped, ch),
            StripState::SawR => step_saw_r(ch, out),
            StripState::RawHashes { count } => step_raw_hashes(count, ch, out),
            StripState::InRaw { needed } => step_in_raw(needed, ch),
            StripState::RawClose { needed, matched } => step_raw_close(needed, matched, ch),
        };
    }

    fn finish_line(&mut self, out: &mut String) {
        self.state = match self.state {
            StripState::Block { depth, .. } => StripState::Block {
                depth,
                saw_star: false,
                saw_slash: false,
            },
            StripState::InRaw { needed } | StripState::RawClose { needed, .. } => {
                StripState::InRaw { needed }
            }
            StripState::PendingSlash => {
                out.push('/');
                StripState::Normal
            }
            StripState::SawQuote => {
                out.push('\'');
                StripState::Normal
            }
            StripState::CharOrLifetime { buffered } => {
                out.push('\'');
                out.push(buffered);
                StripState::Normal
            }
            StripState::SawR => {
                out.push('r');
                StripState::Normal
            }
            StripState::RawHashes { count } => {
                out.push('r');
                for _ in 0..count {
                    out.push('#');
                }
                StripState::Normal
            }
            StripState::Normal | StripState::LineComment | StripState::InChar { .. } => {
                StripState::Normal
            }
            StripState::InString { .. } => StripState::InString { escaped: false },
        };
    }
}

fn step_normal(ch: char, out: &mut String) -> StripState {
    match ch {
        '/' => StripState::PendingSlash,
        '"' => StripState::InString { escaped: false },
        '\'' => StripState::SawQuote,
        'r' => StripState::SawR,
        _ => {
            out.push(ch);
            StripState::Normal
        }
    }
}

fn step_pending_slash(ch: char, out: &mut String) -> StripState {
    if ch == '/' {
        return StripState::LineComment;
    }
    if ch == '*' {
        return StripState::Block {
            depth: 1,
            saw_star: false,
            saw_slash: false,
        };
    }
    out.push('/');
    step_normal(ch, out)
}

fn step_block(depth: usize, saw_star: bool, saw_slash: bool, ch: char) -> StripState {
    if saw_slash && ch == '*' {
        return StripState::Block {
            depth: depth.saturating_add(1),
            saw_star: false,
            saw_slash: false,
        };
    }
    if saw_star && ch == '/' {
        let depth = depth.saturating_sub(1);
        if depth == 0 {
            return StripState::Normal;
        }
        return StripState::Block {
            depth,
            saw_star: false,
            saw_slash: false,
        };
    }
    StripState::Block {
        depth,
        saw_star: ch == '*',
        saw_slash: ch == '/',
    }
}

fn step_string(escaped: bool, ch: char) -> StripState {
    if escaped {
        return StripState::InString { escaped: false };
    }
    match ch {
        '\\' => StripState::InString { escaped: true },
        '"' => StripState::Normal,
        _ => StripState::InString { escaped: false },
    }
}

fn step_saw_quote(ch: char) -> StripState {
    if ch == '\\' {
        return StripState::InChar { escaped: true };
    }
    StripState::CharOrLifetime { buffered: ch }
}

fn step_char_or_lifetime(buffered: char, ch: char, out: &mut String) -> StripState {
    if ch == '\'' {
        return StripState::Normal;
    }
    out.push('\'');
    out.push(buffered);
    step_normal(ch, out)
}

fn step_char(escaped: bool, ch: char) -> StripState {
    if escaped {
        return StripState::InChar { escaped: false };
    }
    match ch {
        '\\' => StripState::InChar { escaped: true },
        '\'' => StripState::Normal,
        _ => StripState::InChar { escaped: false },
    }
}

fn step_saw_r(ch: char, out: &mut String) -> StripState {
    if ch == '#' {
        return StripState::RawHashes { count: 1 };
    }
    if ch == '"' {
        return StripState::InRaw { needed: 0 };
    }
    out.push('r');
    step_normal(ch, out)
}

fn step_raw_hashes(count: usize, ch: char, out: &mut String) -> StripState {
    if ch == '#' {
        return StripState::RawHashes {
            count: count.saturating_add(1),
        };
    }
    if ch == '"' {
        return StripState::InRaw { needed: count };
    }
    out.push('r');
    for _ in 0..count {
        out.push('#');
    }
    step_normal(ch, out)
}

fn step_in_raw(needed: usize, ch: char) -> StripState {
    if ch != '"' {
        return StripState::InRaw { needed };
    }
    if needed == 0 {
        return StripState::Normal;
    }
    StripState::RawClose { needed, matched: 0 }
}

fn step_raw_close(needed: usize, matched: usize, ch: char) -> StripState {
    if ch == '#' {
        let matched = matched.saturating_add(1);
        if matched == needed {
            return StripState::Normal;
        }
        return StripState::RawClose { needed, matched };
    }
    if ch == '"' {
        return StripState::RawClose { needed, matched: 0 };
    }
    StripState::InRaw { needed }
}

fn inside_region(hit: &SearchHit, region: &Region) -> bool {
    hit.file == region.file && hit.line >= region.start && hit.line <= region.end
}

fn cap_sorted(mut hits: Vec<SearchHit>, cap: usize) -> Vec<SearchHit> {
    hits.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
    hits.dedup();
    hits.truncate(cap);
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    use crate::contracts::{DiffSource, FileSnapshot, PinnedRange, RegionKind};
    use crate::gateway::local::LocalGateway;

    fn scratch(name: &str) -> Result<PathBuf, DtError> {
        let dir =
            std::env::temp_dir().join(format!("difftrace-flow-{name}-{}", std::process::id()));
        let _unused = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|source| DtError::Config(source.to_string()))?;
        git(&dir, &["init", "-b", "main"])?;
        Ok(dir)
    }

    fn git(dir: &Path, args: &[&str]) -> Result<(), DtError> {
        let output = std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .map_err(|source| DtError::Config(source.to_string()))?;
        if !output.status.success() {
            return Err(DtError::Config(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        Ok(())
    }

    fn commit(dir: &Path, file: &str, body: &str, message: &str) -> Result<(), DtError> {
        let path = dir.join(file);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|source| DtError::Config(source.to_string()))?;
        }
        std::fs::write(&path, body).map_err(|source| DtError::Config(source.to_string()))?;
        git(dir, &["add", "-A"])?;
        git(
            dir,
            &[
                "-c",
                "user.email=difftrace@example.invalid",
                "-c",
                "user.name=difftrace",
                "commit",
                "-m",
                message,
            ],
        )
    }

    fn region(file: &str, start: u32, end: u32) -> Region {
        Region {
            file: file.to_owned(),
            start,
            end,
            kind: RegionKind::Added,
        }
    }

    #[test]
    fn two_builds_on_the_same_input_are_equal() -> Result<(), DtError> {
        let dir = scratch("equal")?;
        commit(&dir, "src/lib.rs", "fn alpha() {}\n", "base")?;
        commit(
            &dir,
            "src/caller.rs",
            "fn user() {\n    alpha();\n}\n",
            "call",
        )?;
        let gateway = LocalGateway::open_at(&dir, "main")?;
        let target = region("src/lib.rs", 1, 1);
        let first = build(&target, "fn alpha() {}\n", &gateway, 20)?;
        let second = build(&target, "fn alpha() {}\n", &gateway, 20)?;
        assert_eq!(first, second, "identical inputs build identical maps");
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn callers_are_capped_sorted_and_exclude_the_definition() -> Result<(), DtError> {
        let dir = scratch("cap")?;
        commit(&dir, "src/lib.rs", "fn gamma() {}\n", "base")?;
        commit(&dir, "src/a.rs", "fn one() {\n    gamma();\n}\n", "a")?;
        commit(&dir, "src/b.rs", "fn two() {\n    gamma();\n}\n", "b")?;
        commit(&dir, "src/c.rs", "fn three() {\n    gamma();\n}\n", "c")?;
        let gateway = LocalGateway::open_at(&dir, "main")?;
        let target = region("src/lib.rs", 1, 1);
        let map = build(&target, "fn gamma() {}\n", &gateway, 2)?;
        assert_eq!(map.callers.len(), 2, "the cap holds: {:?}", map.callers);
        let files: Vec<&str> = map.callers.iter().map(|hit| hit.file.as_str()).collect();
        assert_eq!(
            files,
            vec!["src/a.rs", "src/b.rs"],
            "sorted (file, line) order keeps the lowest hits"
        );
        assert!(
            map.callers
                .iter()
                .all(|hit| !(hit.file == "src/lib.rs" && hit.line == 1)),
            "the definition line never counts as its own caller"
        );
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn file_regions_number_definitions_from_the_real_line() -> Result<(), DtError> {
        let dir = scratch("offset")?;
        let mut lines: Vec<String> = Vec::new();
        for index in 1..=63u32 {
            if index == 10 {
                lines.push("fn alpha() {}".to_owned());
            } else if (60..=63).contains(&index) {
                lines.push(format!("pub fn work{index}() {{}}"));
            } else {
                lines.push(format!("// filler {index}"));
            }
        }
        let body = format!("{}\n", lines.join("\n"));
        commit(&dir, "src/lib.rs", &body, "base")?;
        commit(
            &dir,
            "src/user.rs",
            "fn user() {\n    alpha();\n}\n",
            "user",
        )?;
        let gateway = LocalGateway::open_at(&dir, "main")?;
        let snapshot = gateway.read_file_at_head("src/lib.rs")?;
        let target = Region {
            file: "src/lib.rs".to_owned(),
            start: 60,
            end: 63,
            kind: RegionKind::File,
        };
        let map = build(&target, &snapshot.content, &gateway, 20)?;
        assert!(
            map.defines.iter().any(|name| name == "alpha"),
            "the definition before the region is still found: {:?}",
            map.defines
        );
        assert!(
            map.callers
                .iter()
                .all(|hit| !(hit.file == "src/lib.rs" && hit.line == 10)),
            "the definition at its true file line never counts as its own caller: {:?}",
            map.callers
        );
        assert!(
            map.callers
                .iter()
                .any(|hit| hit.file == "src/user.rs" && hit.line == 2),
            "the real caller is anchored at its true line: {:?}",
            map.callers
        );
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn trigger_sites_cover_spawn_await_and_trait_impls() -> Result<(), DtError> {
        let dir = scratch("trigger")?;
        commit(&dir, "src/lib.rs", "fn handler() {}\n", "base")?;
        commit(
            &dir,
            "src/await.rs",
            "fn a() {\n    handler().await;\n}\n",
            "await",
        )?;
        commit(
            &dir,
            "src/spawn.rs",
            "fn b() {\n    tokio::spawn(handler());\n}\n",
            "spawn",
        )?;
        commit(
            &dir,
            "src/plain.rs",
            "fn c() {\n    handler();\n}\n",
            "plain",
        )?;
        commit(
            &dir,
            "src/other.rs",
            "impl Sink for Client {\n    fn handler(&self) {}\n}\n",
            "impl",
        )?;
        let gateway = LocalGateway::open_at(&dir, "main")?;
        let target = region("src/lib.rs", 1, 1);
        let map = build(&target, "fn handler() {}\n", &gateway, 20)?;
        let trigger_files: Vec<&str> = map.triggers.iter().map(|hit| hit.file.as_str()).collect();
        assert_eq!(
            trigger_files,
            vec!["src/await.rs", "src/other.rs", "src/spawn.rs"],
            "spawn, await, and trait-impl sites all land in triggers: {:?}",
            map.triggers
        );
        let caller_files: Vec<&str> = map.callers.iter().map(|hit| hit.file.as_str()).collect();
        assert_eq!(
            caller_files,
            vec!["src/plain.rs"],
            "the plain caller stays a caller, not a trigger"
        );
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn defines_scan_fn_async_fn_and_impl_methods() {
        let text = "fn alpha() {}\nasync fn beta() {}\npub fn gamma(x: u32) {}\nimpl Thing {\n    fn delta(&self) {}\n}\nlet sum = compute(1);\n";
        let defines = scan_defines(text);
        let names: Vec<&str> = defines.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            vec!["alpha", "beta", "gamma", "delta"],
            "the fn token in any modifier shape is a define; call tokens are not"
        );
    }

    #[test]
    fn callees_collect_call_tokens_without_keywords() {
        let text = "fn run() {\n    prepare();\n    if ready {\n        check();\n    }\n    finish();\n}\n";
        let defines = scan_defines(text);
        let callees = scan_callees(text, &defines);
        assert_eq!(
            callees,
            vec![
                "prepare".to_owned(),
                "check".to_owned(),
                "finish".to_owned()
            ],
            "call tokens gather in line order minus keywords and own defines"
        );
    }

    struct CountingGateway {
        searches: std::sync::atomic::AtomicUsize,
    }

    impl PrGateway for CountingGateway {
        fn pinned_range(&self) -> PinnedRange {
            PinnedRange {
                base_sha: "base".to_owned(),
                head_sha: "head".to_owned(),
            }
        }
        fn diff(&self) -> DiffSource {
            DiffSource::Unified(String::new())
        }
        fn read_file_at_head(&self, _path: &str) -> Result<FileSnapshot, DtError> {
            Err(DtError::Gateway("unused by the flow map".to_owned()))
        }
        fn list_files_at_head(&self) -> Result<Vec<String>, DtError> {
            Ok(Vec::new())
        }
        fn search_at_head(&self, _pattern: &str, _cap: usize) -> Result<Vec<SearchHit>, DtError> {
            let _bumped = self
                .searches
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(Vec::new())
        }
    }

    #[test]
    fn a_region_without_defines_builds_an_empty_map_without_searching() -> Result<(), DtError> {
        let gateway = CountingGateway {
            searches: std::sync::atomic::AtomicUsize::new(0),
        };
        let target = region("src/notes.md", 1, 1);
        let map = build(&target, "# notes\n", &gateway, 20)?;
        assert!(
            map.defines.is_empty()
                && map.callers.is_empty()
                && map.callees.is_empty()
                && map.triggers.is_empty(),
            "no defines means an empty map: {map:?}"
        );
        assert_eq!(
            gateway.searches.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "the fast path never touches the gateway"
        );
        Ok(())
    }

    #[test]
    fn a_flow_map_round_trips_through_serde() -> Result<(), DtError> {
        let map = FlowMap {
            defines: vec!["alpha".to_owned()],
            callers: vec![SearchHit {
                file: "src/a.rs".to_owned(),
                line: 2,
                text: "    alpha();".to_owned(),
            }],
            callees: vec!["beta".to_owned()],
            triggers: Vec::new(),
        };
        let text =
            serde_json::to_string(&map).map_err(|source| DtError::Config(source.to_string()))?;
        let back: FlowMap =
            serde_json::from_str(&text).map_err(|source| DtError::Config(source.to_string()))?;
        assert_eq!(map, back, "the map survives a serde round trip");
        Ok(())
    }

    #[test]
    fn a_same_name_redefinition_is_not_its_own_caller() -> Result<(), DtError> {
        let dir = scratch("dup")?;
        let body = "impl A {\n    fn same() {}\n}\nimpl B {\n    fn same() {}\n}\n";
        commit(&dir, "src/dup.rs", body, "base")?;
        let gateway = LocalGateway::open_at(&dir, "main")?;
        let target = region("src/dup.rs", 1, 6);
        let map = build(&target, body, &gateway, 20)?;
        assert_eq!(
            map.defines,
            vec!["same".to_owned()],
            "the name lists once: {:?}",
            map.defines
        );
        assert!(
            map.callers.is_empty(),
            "neither definition line reports as a caller: {:?}",
            map.callers
        );
        let _unused = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn commented_and_string_fn_tokens_are_not_defines() {
        let text = "// fn ghost(\nlet s = \"call fn fake(\";\n/* fn hidden( still\n   hidden */\nfn real() {}\n/// fn documented(\nfn live() {}\n";
        let defines = scan_defines(text);
        let names: Vec<&str> = defines.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            vec!["real", "live"],
            "commented, string, and block-commented fn tokens never define: {names:?}"
        );
    }

    #[test]
    fn comments_strings_and_the_in_keyword_stay_out_of_callees() {
        let text = "fn run() {\n    for i in(0..3) {\n        step(i);\n    }\n    // config.load(x)\n    let s = \"prepare(3)\";\n}\n";
        let defines = scan_defines(text);
        let callees = scan_callees(text, &defines);
        assert_eq!(
            callees,
            vec!["step".to_owned()],
            "in(, commented calls, and string contents are not callees: {callees:?}"
        );
    }

    #[test]
    fn nested_block_comments_strip_to_their_depth() {
        let text = "/* outer /* inner */ still\n   commented */ fn real() {}\nlet x = 1;\n";
        let defines = scan_defines(text);
        let names: Vec<&str> = defines.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            vec!["real"],
            "an inner close does not end the comment; the depth-matching close does"
        );
        let nested = "/* outer /* inner */ phantom(1); */ real(2);\n";
        let defines = scan_defines(nested);
        let callees = scan_callees(nested, &defines);
        assert_eq!(
            callees,
            vec!["real".to_owned()],
            "no callee leaks from commented-out code between the inner and outer closes"
        );
    }

    #[test]
    fn char_literals_and_lifetimes_strip_correctly() {
        let text = "let q = '\"'; helper(x);\nfn visible() {}\nlet nl = '\\n';\nfn scope<'a>(x: &'a u32) {}\n";
        let defines = scan_defines(text);
        let names: Vec<&str> = defines.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            vec!["visible", "scope"],
            "a quote char never opens a string and a lifetime never swallows code"
        );
        let callees = scan_callees(text, &defines);
        assert_eq!(
            callees,
            vec!["helper".to_owned()],
            "the call after a quote char survives on the same line"
        );
    }

    #[test]
    fn raw_strings_strip_with_hash_counting() {
        let text = "let raw = r#\"inner \"quoted\" fn fake(\"#;\nfn real() {}\nlet multi = r\"\nfn also_fake(\n\";\nfn live() {}\nlet s = r#\"x\\\"#; after(3);\n";
        let defines = scan_defines(text);
        let names: Vec<&str> = defines.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            vec!["real", "live"],
            "inner quotes do not close a hashed raw string and a raw string spans lines"
        );
        let callees = scan_callees(text, &defines);
        assert_eq!(
            callees,
            vec!["after".to_owned()],
            "a backslash never escapes inside a raw string, so the closer still closes"
        );
    }

    #[test]
    fn unterminated_strings_carry_across_the_line_break() {
        let continued = "let s = \"a\\\nb\"; helper(x);\nfn real() {}\n";
        let defines = scan_defines(continued);
        let names: Vec<&str> = defines.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            vec!["real"],
            "the continuation line resumes inside the string, so no phantom define mints"
        );
        let callees = scan_callees(continued, &defines);
        assert_eq!(
            callees,
            vec!["helper".to_owned()],
            "the callee after the closing quote on the continuation line survives"
        );
        let multiline = "let q = \"one\ntwo\"; after(1);\nfn live() {}\n";
        let defines = scan_defines(multiline);
        let names: Vec<&str> = defines.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            vec!["live"],
            "a plain multiline string — legal Rust — carries the same way"
        );
        let callees = scan_callees(multiline, &defines);
        assert_eq!(
            callees,
            vec!["after".to_owned()],
            "code after a string that closed on a later line still reads as code"
        );
    }
}

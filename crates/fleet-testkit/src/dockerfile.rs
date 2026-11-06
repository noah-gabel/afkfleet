//! A reader of the repository's Dockerfiles, for the tests that pin them
//! (ADR-0014; ADR-0022, S13): fleet-agent's and fleet-server's
//! `tests/deploy.rs` read `deploy/docker/*.Dockerfile` through it, so both
//! images are checked the same way.
//!
//! It knows only what those files use: one instruction per line, a line
//! ending in `\` continued on the next, `#` comments on lines of their own,
//! and stages opened by `FROM … AS name`. Anything it can't place, it
//! reports as missing rather than guessing.

/// `text`'s instructions, each on one line: blank lines and comments left
/// out, a line ending in `\` joined to the next with one space.
#[must_use]
pub fn instructions(text: &str) -> Vec<String> {
    let mut instructions = Vec::new();
    let mut current = String::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(continued) = line.strip_suffix('\\') {
            current.push_str(continued.trim_end());
            current.push(' ');
        } else {
            current.push_str(line);
            instructions.push(core::mem::take(&mut current));
        }
    }
    instructions
}

/// Every instruction of `text` that is a `keyword` one (`CMD` finds `CMD
/// […]`, never `HEALTHCHECK … CMD […]`).
#[must_use]
pub fn every(text: &str, keyword: &str) -> Vec<String> {
    let prefix = format!("{keyword} ");
    instructions(text)
        .into_iter()
        .filter(|instruction| instruction.starts_with(&prefix))
        .collect()
}

/// The one `keyword` instruction of `text`; `None` when there's none or
/// more than one.
#[must_use]
pub fn only(text: &str, keyword: &str) -> Option<String> {
    let mut found = every(text, keyword).into_iter();
    match (found.next(), found.next()) {
        (Some(one), None) => Some(one),
        _ => None,
    }
}

/// The stage `name` of `text`: its `FROM … AS name` and every instruction
/// up to the next `FROM`; `None` when no stage has that name.
#[must_use]
pub fn stage(text: &str, name: &str) -> Option<Vec<String>> {
    let suffix = format!(" AS {name}");
    let mut found = instructions(text).into_iter().skip_while(|instruction| {
        !(instruction.starts_with("FROM ") && instruction.ends_with(&suffix))
    });
    let from = found.next()?;
    let mut stage = vec![from];
    stage.extend(found.take_while(|instruction| !instruction.starts_with("FROM ")));
    Some(stage)
}

/// The last stage of `text`, the image itself: its `FROM` and everything
/// after it; `None` without a `FROM`.
#[must_use]
pub fn final_stage(text: &str) -> Option<Vec<String>> {
    let all = instructions(text);
    let start = all
        .iter()
        .rposition(|instruction| instruction.starts_with("FROM "))?;
    all.get(start..).map(<[String]>::to_vec)
}

/// The image a `FROM` instruction names, without its `AS` part: `FROM
/// rust:1@sha256:… AS chef` gives `rust:1@sha256:…`.
#[must_use]
pub fn from_image(instruction: &str) -> Option<&str> {
    instruction.strip_prefix("FROM ")?.split_whitespace().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "\
# A comment.
FROM base:1@sha256:abc AS chef
RUN one \\
    two

FROM chef AS planner
COPY a b
# Another comment.
FROM other:2@sha256:def
USER 65532:65532
CMD [\"run\"]
HEALTHCHECK --interval=10s \\
    CMD [\"check\"]
";

    #[test]
    fn instructions_join_continuations_and_drop_comments() {
        assert_eq!(
            instructions(FILE),
            [
                "FROM base:1@sha256:abc AS chef",
                "RUN one two",
                "FROM chef AS planner",
                "COPY a b",
                "FROM other:2@sha256:def",
                "USER 65532:65532",
                "CMD [\"run\"]",
                "HEALTHCHECK --interval=10s CMD [\"check\"]",
            ]
        );
    }

    #[test]
    fn a_keyword_finds_only_its_own_instructions() {
        assert_eq!(every(FILE, "CMD"), ["CMD [\"run\"]"]);
        assert_eq!(only(FILE, "USER").as_deref(), Some("USER 65532:65532"));
        assert_eq!(every(FILE, "FROM").len(), 3);
        assert_eq!(only(FILE, "FROM"), None, "more than one");
        assert_eq!(only(FILE, "VOLUME"), None, "none");
    }

    #[test]
    fn a_stage_runs_from_its_from_to_the_next() {
        assert_eq!(
            stage(FILE, "chef"),
            Some(vec![
                "FROM base:1@sha256:abc AS chef".to_owned(),
                "RUN one two".to_owned()
            ])
        );
        assert_eq!(
            stage(FILE, "planner"),
            Some(vec![
                "FROM chef AS planner".to_owned(),
                "COPY a b".to_owned()
            ])
        );
        assert_eq!(stage(FILE, "builder"), None);
    }

    #[test]
    fn the_final_stage_is_the_last_from_onwards() {
        assert_eq!(
            final_stage(FILE).map(|stage| stage.len()),
            Some(4),
            "{:?}",
            final_stage(FILE)
        );
        assert_eq!(final_stage("USER x"), None);
    }

    #[test]
    fn a_from_names_its_image_without_the_stage() {
        assert_eq!(
            from_image("FROM base:1@sha256:abc AS chef"),
            Some("base:1@sha256:abc")
        );
        assert_eq!(from_image("FROM other:2"), Some("other:2"));
        assert_eq!(from_image("RUN x"), None);
    }
}

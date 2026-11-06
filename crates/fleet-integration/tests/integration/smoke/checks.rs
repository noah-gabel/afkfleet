//! What the smoke tests read from docker's output, kept apart from Docker
//! so their own unit tests run with every `just test`.

use fleet_testkit::docker::Finished;

/// Everything the stack and every call printed, which no secret may be in
/// (ADR-0020, S28, S127; ADR-0022, S19), and the secrets to look for.
#[derive(Debug, Default)]
pub(crate) struct Transcript {
    texts: Vec<String>,
    secrets: Vec<String>,
}

impl Transcript {
    /// Keeps `finished`'s stdout and stderr.
    pub(crate) fn keep(&mut self, finished: &Finished) {
        self.texts.push(finished.stdout.clone());
        self.texts.push(finished.stderr.clone());
    }

    /// Keeps `finished`'s stderr alone: its stdout is a secret.
    pub(crate) fn keep_stderr(&mut self, finished: &Finished) {
        self.texts.push(finished.stderr.clone());
    }

    /// Keeps `text`, a log.
    pub(crate) fn keep_text(&mut self, text: String) {
        self.texts.push(text);
    }

    /// Adds `secret`, which nothing kept may hold.
    pub(crate) fn forbid(&mut self, secret: &str) {
        if !secret.is_empty() {
            self.secrets.push(secret.to_owned());
        }
    }

    /// Fails, naming the secret's number and the text's but never either,
    /// when anything kept holds a forbidden secret or a query parameter of
    /// the live events' or the share search's.
    pub(crate) fn assert_clean(&self) {
        for (s, secret) in self.secrets.iter().enumerate() {
            for (t, text) in self.texts.iter().enumerate() {
                assert!(
                    !text.contains(secret.as_str()),
                    "secret #{s} is in kept text #{t}"
                );
            }
        }
        for (t, text) in self.texts.iter().enumerate() {
            for parameter in ["ticket=", "q="] {
                assert!(
                    !has_parameter(text, parameter),
                    "kept text #{t} holds a {parameter} parameter"
                );
            }
        }
    }
}

/// Whether `text` holds the query parameter `parameter` (`ticket=`): after
/// a `?` or an `&`, or `&` in JSON.
fn has_parameter(text: &str, parameter: &str) -> bool {
    ["?", "&", "\\u0026"]
        .iter()
        .any(|before| text.contains(&format!("{before}{parameter}")))
}

/// `docker stats --format '{{.MemPerc}} {{.PIDs}}'`'s line: the memory used
/// as a percentage of the limit, and the process count.
pub(crate) fn stats(line: &str) -> Option<(f64, u64)> {
    let (memory, pids) = line.trim().split_once(' ')?;
    Some((
        memory.trim_end_matches('%').parse().ok()?,
        pids.trim().parse().ok()?,
    ))
}

/// `docker inspect --format '{{range .Mounts}}{{.Type}}:{{.Destination}} {{end}}'`'s
/// mounts, as (type, destination).
pub(crate) fn mounts(line: &str) -> Vec<(String, String)> {
    line.split_whitespace()
        .filter_map(|mount| mount.split_once(':'))
        .map(|(kind, destination)| (kind.to_owned(), destination.to_owned()))
        .collect()
}

/// `/proc/<pid>/status`'s capability sets, by name (`CapEff`), as hex.
pub(crate) fn capabilities(status: &str) -> Vec<(String, String)> {
    status
        .lines()
        .filter_map(|line| line.split_once(':'))
        .filter(|(name, _)| name.starts_with("Cap"))
        .map(|(name, value)| (name.to_owned(), value.trim().to_owned()))
        .collect()
}

/// A series' value in a Prometheus render, `None` when it isn't there.
pub(crate) fn series(render: &str, name: &str) -> Option<f64> {
    render.lines().find_map(|line| {
        line.strip_prefix(name)
            .and_then(|rest| rest.strip_prefix(' '))
            .and_then(|value| value.trim().parse().ok())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finished(stdout: &str, stderr: &str) -> Finished {
        Finished {
            success: true,
            code: Some(0),
            stdout: stdout.to_owned(),
            stderr: stderr.to_owned(),
        }
    }

    #[test]
    fn a_clean_transcript_passes() {
        let mut transcript = Transcript::default();
        transcript.keep(&finished("GET /api/v1/events 502", "uri=/api/v1/events"));
        transcript.forbid("hunter2-secret");

        transcript.assert_clean();
    }

    #[test]
    #[should_panic(expected = "secret #0 is in kept text #1")]
    fn a_secret_anywhere_fails_without_being_named() {
        let mut transcript = Transcript::default();
        transcript.keep(&finished("", "the password was hunter2-secret"));
        transcript.forbid("hunter2-secret");

        transcript.assert_clean();
    }

    #[test]
    fn the_secret_stdout_isnt_kept() {
        let mut transcript = Transcript::default();
        transcript.keep_stderr(&finished("bundle-secret", "a token is created"));
        transcript.forbid("bundle-secret");

        transcript.assert_clean();
    }

    #[test]
    #[should_panic(expected = "holds a ticket= parameter")]
    fn a_ticket_in_a_logged_uri_fails() {
        let mut transcript = Transcript::default();
        transcript.keep_text(r#"{"uri":"/api/v1/events?ticket=afk_wt_x"}"#.to_owned());

        transcript.assert_clean();
    }

    #[test]
    #[should_panic(expected = "holds a q= parameter")]
    fn a_q_after_another_parameter_fails() {
        let mut transcript = Transcript::default();
        transcript.keep_text(r#"{"uri":"/x?limit=5&q=pass"}"#.to_owned());

        transcript.assert_clean();
    }

    #[test]
    fn stats_read_percent_and_pids() {
        assert_eq!(stats("12.34% 17\n"), Some((12.34, 17)));
        assert_eq!(stats("--"), None);
    }

    #[test]
    fn mounts_read_type_and_destination() {
        assert_eq!(
            mounts("volume:/data bind:/backups bind:/etc/afkfleet/server.toml \n"),
            [
                ("volume".to_owned(), "/data".to_owned()),
                ("bind".to_owned(), "/backups".to_owned()),
                ("bind".to_owned(), "/etc/afkfleet/server.toml".to_owned()),
            ]
        );
    }

    #[test]
    fn capabilities_read_every_set() {
        let status =
            "Name:\tcaddy\nCapInh:\t0000000000000000\nCapBnd:\t0000000000000400\nNoNewPrivs:\t1\n";

        assert_eq!(
            capabilities(status),
            [
                ("CapInh".to_owned(), "0000000000000000".to_owned()),
                ("CapBnd".to_owned(), "0000000000000400".to_owned()),
            ]
        );
    }

    #[test]
    fn a_series_reads_its_value() {
        let render = "# TYPE a counter\na_total{x=\"y\"} 3\nb 1.5\n";

        assert_eq!(series(render, "a_total{x=\"y\"}"), Some(3.0));
        assert_eq!(series(render, "b"), Some(1.5));
        assert_eq!(series(render, "c"), None);
    }
}

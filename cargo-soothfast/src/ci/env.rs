//! What a CI job knows about the run it is in, from the runner's environment
//! or from flags when there is none.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::Value;

const NO_COMMIT: &str = "0000000000000000000000000000000000000000";

/// The event that started the run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    PullRequest,
    Push,
    Other,
}

impl Event {
    fn parse(name: &str) -> Event {
        match name {
            "pull_request" => Event::PullRequest,
            "push" => Event::Push,
            _ => Event::Other,
        }
    }
}

/// The flags every `ci` kind shares. A flag overrides what the environment
/// says.
#[derive(Default, Debug, PartialEq)]
pub struct Flags {
    pub event: Option<Event>,
    pub base: Option<String>,
    pub results: Option<PathBuf>,
}

/// The run a job is part of.
#[derive(Debug, PartialEq)]
pub struct CiEnv {
    pub event: Event,
    /// The commit changes are measured from: a pull request's base, a push's
    /// previous head.
    pub base: Option<String>,
    pub head: Option<String>,
    pub default_branch: bool,
    /// Where jobs record their verdicts. A run with none records nothing.
    pub results: Option<PathBuf>,
    pub pr: Option<u64>,
}

/// Takes the flags `--event`, `--base` and `--results` out of `args`, and
/// returns them with the arguments left, in order. Arguments after `--` are
/// left alone.
pub fn split_flags(args: &[String]) -> Result<(Flags, Vec<String>), String> {
    let mut flags = Flags::default();
    let mut rest = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (a.as_str(), None),
        };
        let mut value = || {
            inline
                .clone()
                .or_else(|| it.next().cloned())
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match flag {
            "--event" => flags.event = Some(Event::parse(&value()?)),
            "--base" => flags.base = Some(value()?),
            "--results" => flags.results = Some(PathBuf::from(value()?)),
            "--" => {
                rest.push(a.clone());
                rest.extend(it.cloned());
                break;
            }
            _ => rest.push(a.clone()),
        }
    }
    Ok((flags, rest))
}

impl CiEnv {
    /// Reads the process environment and the event payload the runner names.
    pub fn from_process(flags: &Flags) -> CiEnv {
        let vars: BTreeMap<String, String> = std::env::vars().collect();
        let payload = vars
            .get("GITHUB_EVENT_PATH")
            .and_then(|p| std::fs::read_to_string(p).ok());
        CiEnv::parse(&vars, payload.as_deref(), flags)
    }

    /// Builds the environment from `vars` and the event `payload` JSON.
    pub fn parse(vars: &BTreeMap<String, String>, payload: Option<&str>, flags: &Flags) -> CiEnv {
        let payload: Value = payload
            .and_then(|p| serde_json::from_str(p).ok())
            .unwrap_or(Value::Null);
        let var = |k: &str| vars.get(k).filter(|v| !v.is_empty());
        let text = |path: &[&str]| {
            let leaf = path.iter().try_fold(&payload, |v, k| v.get(k))?;
            leaf.as_str().map(String::from)
        };
        let event = flags
            .event
            .or_else(|| var("GITHUB_EVENT_NAME").map(|n| Event::parse(n)))
            .unwrap_or(Event::Other);
        let from_event = match event {
            Event::PullRequest => text(&["pull_request", "base", "sha"]),
            Event::Push => text(&["before"]).filter(|b| b != NO_COMMIT),
            Event::Other => None,
        };
        let default_branch = text(&["repository", "default_branch"])
            .zip(var("GITHUB_REF"))
            .is_some_and(|(d, r)| *r == format!("refs/heads/{d}"));
        CiEnv {
            event,
            base: flags.base.clone().or(from_event),
            head: text(&["pull_request", "head", "sha"]).or_else(|| var("GITHUB_SHA").cloned()),
            default_branch,
            results: flags
                .results
                .clone()
                .or_else(|| var("RUNNER_TEMP").map(|t| PathBuf::from(t).join("soothfast-results"))),
            pr: payload
                .pointer("/pull_request/number")
                .and_then(Value::as_u64),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn argv(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    const PULL: &str = r#"{"number": 7, "pull_request": {"number": 7,
        "base": {"sha": "b4se"}, "head": {"sha": "h3ad"}}}"#;
    const PUSH: &str = r#"{"before": "pr3v", "repository": {"default_branch": "main"}}"#;

    #[test]
    fn github_pull_request_env() {
        let env = CiEnv::parse(
            &vars(&[
                ("GITHUB_EVENT_NAME", "pull_request"),
                ("GITHUB_SHA", "merge"),
                ("RUNNER_TEMP", "/tmp/r"),
            ]),
            Some(PULL),
            &Flags::default(),
        );
        assert_eq!(
            env,
            CiEnv {
                event: Event::PullRequest,
                base: Some("b4se".into()),
                head: Some("h3ad".into()),
                default_branch: false,
                results: Some("/tmp/r/soothfast-results".into()),
                pr: Some(7),
            }
        );
    }

    #[test]
    fn github_push_to_default_branch() {
        let env = CiEnv::parse(
            &vars(&[
                ("GITHUB_EVENT_NAME", "push"),
                ("GITHUB_SHA", "tip"),
                ("GITHUB_REF", "refs/heads/main"),
            ]),
            Some(PUSH),
            &Flags::default(),
        );
        assert_eq!(env.event, Event::Push);
        assert_eq!(env.base.as_deref(), Some("pr3v"));
        assert_eq!(env.head.as_deref(), Some("tip"));
        assert!(env.default_branch);
        assert_eq!(env.results, None);
        assert_eq!(env.pr, None);
    }

    #[test]
    fn push_of_a_new_branch_has_no_base() {
        let payload = format!(r#"{{"before": "{NO_COMMIT}"}}"#);
        let env = CiEnv::parse(
            &vars(&[("GITHUB_EVENT_NAME", "push")]),
            Some(&payload),
            &Flags::default(),
        );
        assert_eq!(env.base, None);
    }

    #[test]
    fn flags_alone_describe_a_local_run() {
        let (flags, rest) = split_flags(&argv(&[
            "--event",
            "push",
            "--base=abc",
            "--results",
            "out",
            "test",
        ]))
        .unwrap();
        assert_eq!(rest, ["test"]);
        let env = CiEnv::parse(&vars(&[]), None, &flags);
        assert_eq!(env.event, Event::Push);
        assert_eq!(env.base.as_deref(), Some("abc"));
        assert_eq!(env.results, Some("out".into()));
        assert_eq!(env.head, None);
    }

    #[test]
    fn flags_override_the_environment() {
        let flags = Flags {
            event: Some(Event::Push),
            base: Some("mine".into()),
            results: Some("here".into()),
        };
        let env = CiEnv::parse(
            &vars(&[
                ("GITHUB_EVENT_NAME", "pull_request"),
                ("RUNNER_TEMP", "/tmp/r"),
            ]),
            Some(PULL),
            &flags,
        );
        assert_eq!(env.event, Event::Push);
        assert_eq!(env.base.as_deref(), Some("mine"));
        assert_eq!(env.results, Some("here".into()));
    }

    #[test]
    fn split_flags_leaves_arguments_after_the_separator_alone() {
        let (flags, rest) =
            split_flags(&argv(&["test", "--all", "--base", "x", "--", "--base"])).unwrap();
        assert_eq!(flags.base.as_deref(), Some("x"));
        assert_eq!(rest, ["test", "--all", "--", "--base"]);
        assert!(split_flags(&argv(&["--results"])).is_err());
    }
}

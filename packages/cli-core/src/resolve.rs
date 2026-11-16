//! Resolve human-friendly contest/problem references to the ids the API expects.

use crate::api::contest::ContestListItem;
use crate::client::Client;
use crate::ids::{ContestId, ProblemId};
use anyhow::{Context, bail};

/// Resolve a contest reference (its id, its title or a unique part of it) to
/// the contest's id.
pub fn contest_id(client: &Client, input: &str) -> anyhow::Result<ContestId> {
    let q = input.trim();
    if q.is_empty() {
        bail!("Empty contest reference.");
    }
    if let Some(id) = offline_contest_id(q) {
        return Ok(id);
    }

    // The server's search keeps the contests whose title contains `q`, over
    // every contest the caller can see rather than one page of them.
    let contests = client
        .contests(Some(q))
        .context("Failed to list contests to resolve the name")?;
    let lower = q.to_lowercase();

    let exact = contests_named(&contests, q);
    if exact.len() == 1 {
        return Ok(ContestId::from_server(exact[0].id));
    }
    if exact.len() > 1 {
        bail!(
            "'{}' matches several contests by exact title. Use the numeric id ({}).",
            q,
            exact
                .iter()
                .map(|c| c.id.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let part: Vec<_> = contests
        .iter()
        .filter(|c| c.title.to_lowercase().contains(&lower))
        .collect();
    match part.as_slice() {
        [c] => Ok(ContestId::from_server(c.id)),
        [] => bail!(
            "No contest matching '{}'. Run `broccoli contest list` to see contests.",
            q
        ),
        many => bail!(
            "'{}' matches several contests: {}. Be more specific or use the numeric id.",
            q,
            many.iter()
                .map(|c| format!("{} (#{})", c.title, c.id))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// The contest `input` names with no server to ask: only its id can.
pub fn offline_contest_id(input: &str) -> Option<ContestId> {
    input.trim().parse().ok().map(ContestId::from_server)
}

/// The contests `input` names exactly: by id, or by title ignoring case, the
/// way a contest reference does before it falls back to part of a title.
fn contests_named<'a>(contests: &'a [ContestListItem], input: &str) -> Vec<&'a ContestListItem> {
    let q = input.trim().to_lowercase();
    contests
        .iter()
        .filter(|c| c.id.to_string() == q || c.title.to_lowercase() == q)
        .collect()
}

/// A contest `input` names exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedContest {
    pub id: ContestId,
    pub title: String,
}

/// The contest `input` names exactly, if any: the contest whose id it is,
/// else one whose whole title it is, ignoring case. Unlike [`contest_id`], an
/// id is checked against the server, since text that is only a number may
/// still be meant as something else.
pub fn contest_named(client: &Client, input: &str) -> anyhow::Result<Option<NamedContest>> {
    if let Some(id) = offline_contest_id(input) {
        match client.get_contest(id) {
            Ok(contest) => {
                return Ok(Some(NamedContest {
                    id,
                    title: contest.title,
                }));
            }
            Err(crate::client::ClientError::Api(e)) if e.status == 404 => {}
            Err(e) => return Err(e).context("Failed to look up the contest"),
        }
    }
    let contests = client
        .contests(Some(input))
        .context("Failed to list contests")?;
    Ok(contests_named(&contests, input)
        .first()
        .map(|c| NamedContest {
            id: ContestId::from_server(c.id),
            title: c.title.clone(),
        }))
}

/// A contest problem a reference resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProblem {
    pub id: ProblemId,
    pub label: String,
    pub title: String,
}

impl ResolvedProblem {
    /// The reference that names this problem and nothing else (`#<id>`),
    /// for saving in `.broccoli`.
    pub fn reference(&self) -> String {
        format!("#{}", self.id)
    }
}

impl std::fmt::Display for ResolvedProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.label, self.title)
    }
}

/// The problem id an explicit `#<id>` reference names.
pub fn explicit_problem_id(input: &str) -> Option<ProblemId> {
    input
        .trim()
        .strip_prefix('#')?
        .trim()
        .parse()
        .ok()
        .map(ProblemId::from_server)
}

/// Resolve a reference to a problem outside every contest: its id, as `12`
/// or `#12`. With no contest there are no labels or positions to match.
pub fn standalone_problem_id(input: &str) -> anyhow::Result<ProblemId> {
    explicit_problem_id(input)
        .or_else(|| input.trim().parse().ok().map(ProblemId::from_server))
        .with_context(|| format!("'{}' is not a problem id", input.trim()))
}

/// Resolve a problem reference within a contest. A reference is, in order:
/// `#<id>` (the problem id), the label (`A`), the 1-based position in the
/// contest (`1`), or the title (`Two Sum`, or a unique part of it). A
/// reference that two of these read as different problems is refused.
pub fn problem(
    client: &Client,
    contest: ContestId,
    input: &str,
) -> anyhow::Result<ResolvedProblem> {
    if input.trim().is_empty() {
        bail!("Empty problem reference.");
    }
    let problems = client
        .list_contest_problems(contest)
        .context("Failed to list contest problems to resolve the reference")?;
    if problems.is_empty() {
        bail!(
            "Contest {} has no problems to match '{}'.",
            contest,
            input.trim()
        );
    }
    match_problem(&problems, input).map(|p| ResolvedProblem {
        id: ProblemId::from_server(p.problem_id),
        label: p.label.clone(),
        title: crate::model::problem_title(p).to_string(),
    })
}

/// Pure matcher for [`problem`], split out for unit testing without a server.
fn match_problem<'a>(
    problems: &'a [crate::api::contest::ContestProblemResponse],
    input: &str,
) -> anyhow::Result<&'a crate::api::contest::ContestProblemResponse> {
    let q = input.trim();
    if let Some(id) = explicit_problem_id(q) {
        return problems
            .iter()
            .find(|p| p.problem_id == id.get())
            .with_context(|| {
                format!(
                    "Problem #{id} is not in this contest. Available: {}.",
                    available(problems)
                )
            });
    }

    let lower = q.to_lowercase();
    let by_label = problems.iter().find(|p| p.label.to_lowercase() == lower);
    let by_position = q.parse::<usize>().ok().and_then(|n| {
        let mut ordered: Vec<&_> = problems.iter().collect();
        ordered.sort_by_key(|p| p.position);
        n.checked_sub(1).and_then(|i| ordered.get(i).copied())
    });
    match (by_label, by_position) {
        (Some(a), Some(b)) if a.problem_id != b.problem_id => bail!(
            "'{q}' is both the label of {} ({}) and the position of {} ({}). \
             Use the label, or #<id> for a problem id.",
            a.label,
            crate::model::problem_title(a),
            b.label,
            crate::model::problem_title(b)
        ),
        (Some(p), _) | (None, Some(p)) => return Ok(p),
        (None, None) => {}
    }
    if q.parse::<i64>().is_ok() {
        bail!(
            "This contest has {} problem(s); there is no problem {q}. \
             Use a label, a position from 1 to {}, or #<id> for a problem id. Available: {}.",
            problems.len(),
            problems.len(),
            available(problems)
        );
    }

    if let Some(p) = problems
        .iter()
        .find(|p| p.problem_title.as_deref().map(str::to_lowercase) == Some(lower.clone()))
    {
        return Ok(p);
    }
    let part: Vec<_> = problems
        .iter()
        .filter(|p| {
            p.problem_title
                .as_deref()
                .is_some_and(|t| t.to_lowercase().contains(&lower))
        })
        .collect();
    match part.as_slice() {
        [p] => Ok(p),
        [] => bail!(
            "No problem matching '{}' in this contest. Available: {}.",
            q,
            available(problems)
        ),
        many => bail!(
            "'{}' matches several problems: {}. Use the label.",
            q,
            many.iter()
                .map(|p| format!("{} ({})", p.label, crate::model::problem_title(p)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn available(problems: &[crate::api::contest::ContestProblemResponse]) -> String {
    problems
        .iter()
        .map(|p| format!("{} ({})", p.label, crate::model::problem_title(p)))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::{contests_named, match_problem};
    use crate::api::contest::{ContestListItem, ContestPhase, ContestProblemResponse};

    fn contest(id: i32, title: &str) -> ContestListItem {
        let at = "2030-01-01T00:00:00Z".parse().unwrap();
        ContestListItem {
            id,
            title: title.into(),
            activate_time: None,
            start_time: at,
            end_time: at,
            deactivate_time: None,
            is_public: true,
            submissions_visible: false,
            show_compile_output: true,
            show_participants_list: true,
            contest_type: None,
            created_at: at,
            updated_at: at,
            phase: ContestPhase::Running,
            plugin_outage: None,
        }
    }

    #[test]
    fn a_contest_is_named_by_its_id_or_its_whole_title() {
        let contests = [contest(3, "Spring Round"), contest(4, "Spring Round 2")];
        let ids =
            |q: &str| -> Vec<i32> { contests_named(&contests, q).iter().map(|c| c.id).collect() };
        assert_eq!(ids("spring round "), [3]);
        assert_eq!(ids("4"), [4]);
        assert!(ids("Spring").is_empty());
        assert!(ids("Is Spring Round open to all?").is_empty());
    }

    fn p(problem_id: i32, label: &str, position: i32, title: &str) -> ContestProblemResponse {
        ContestProblemResponse {
            contest_id: 1,
            problem_id,
            label: label.into(),
            position,
            problem_title: Some(title.into()),
            withheld: None,
        }
    }

    fn sample() -> Vec<ContestProblemResponse> {
        // ids unrelated to order; 0-based, gapped positions
        vec![
            p(10, "A", 0, "Two Sum"),
            p(11, "B", 2, "Subarrays"),
            p(12, "C", 5, "Graph Walk"),
        ]
    }

    fn id(problems: &[ContestProblemResponse], q: &str) -> i32 {
        match_problem(problems, q).unwrap().problem_id
    }

    #[test]
    fn label_case_insensitive() {
        assert_eq!(id(&sample(), "A"), 10);
        assert_eq!(id(&sample(), "b"), 11);
    }

    #[test]
    fn a_number_is_the_position_in_the_contest() {
        // not the `position` value, which is 0-based and gapped
        assert_eq!(id(&sample(), "1"), 10);
        assert_eq!(id(&sample(), "2"), 11);
        assert_eq!(id(&sample(), "3"), 12);
    }

    #[test]
    fn a_number_that_is_also_a_problem_id_still_means_the_position() {
        let problems = vec![p(2, "A", 0, "Two Sum"), p(1, "B", 1, "Subarrays")];
        assert_eq!(id(&problems, "1"), 2, "1 is the first problem, A");
        assert_eq!(id(&problems, "#1"), 1, "#1 is the problem with id 1");
    }

    #[test]
    fn label_position_and_title_name_the_same_problem() {
        for q in ["A", "1", "Two Sum", "#10"] {
            assert_eq!(id(&sample(), q), 10, "{q}");
        }
    }

    #[test]
    fn a_reference_read_two_ways_is_refused() {
        let problems = vec![p(10, "2", 0, "First"), p(11, "1", 1, "Second")];
        let err = match_problem(&problems, "1").unwrap_err().to_string();
        assert!(err.contains("both the label"), "{err}");
    }

    #[test]
    fn title_exact_and_substring() {
        assert_eq!(id(&sample(), "Two Sum"), 10);
        assert_eq!(id(&sample(), "graph"), 12);
    }

    #[test]
    fn unknown_errors() {
        assert!(match_problem(&sample(), "ZZZ").is_err());
        assert!(match_problem(&sample(), "99").is_err());
        assert!(match_problem(&sample(), "#99").is_err());
    }
}

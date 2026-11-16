-- Each contest submission as the host shows it. The submission row projects
-- its current judgement, so its status and verdict are the result to read
-- (see `SubmissionRow`), whatever judgements it has.
SELECT s.id AS submission_id, s.user_id, s.problem_id,
       (EXTRACT(EPOCH FROM (s.created_at - c.start_time)) * 1000000)::bigint
           AS submitted_at_us,
       s.status, s.verdict
FROM submission s
JOIN contest c ON c.id = s.contest_id
JOIN contest_user cu ON cu.contest_id = c.id AND cu.user_id = s.user_id
JOIN contest_problem cp ON cp.contest_id = c.id AND cp.problem_id = s.problem_id
WHERE s.contest_id = $1 AND s.contest_type = 'codelink-qualifier'
  AND s.created_at >= c.start_time AND s.created_at <= c.end_time
ORDER BY s.created_at, s.id

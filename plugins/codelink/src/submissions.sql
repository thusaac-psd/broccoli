SELECT s.id AS submission_id, s.user_id, s.problem_id,
       (EXTRACT(EPOCH FROM (s.created_at - c.start_time)) * 1000000)::bigint
           AS submitted_at_us,
       (j.is_finalized AND j.status = 'Judged' AND j.verdict = 'Accepted')
           IS TRUE AS accepted,
       CASE WHEN j.id IS NULL
            THEN s.status IN ('Queued', 'Pending', 'Compiling', 'Running')
            ELSE NOT j.is_finalized
       END AS pending
FROM submission s
JOIN contest c ON c.id = s.contest_id
-- Initial queued submissions have no judgement until the dispatcher claims them.
-- Keep is_current in ON so missing current rows survive the LEFT JOIN.
LEFT JOIN submission_judgement j ON j.submission_id = s.id AND j.is_current = TRUE
JOIN contest_user cu ON cu.contest_id = c.id AND cu.user_id = s.user_id
JOIN contest_problem cp ON cp.contest_id = c.id AND cp.problem_id = s.problem_id
WHERE s.contest_id = $1 AND s.contest_type = 'codelink'
  AND s.created_at >= c.start_time AND s.created_at <= c.end_time
ORDER BY s.created_at, s.id

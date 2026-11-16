#!/usr/bin/env python3
"""Run the production standings query against PostgreSQL temporary fixtures.

Example with the local deployment:
  python3 plugins/codelink-qualifier/tests/check_submissions_query.py \
    --container broccoli-codelink-local-db-1 --user broccoli --database broccoli_codelink

Only connection-local temporary tables are created. ROLLBACK removes the fixtures
without touching the deployment's contest or submission data.
"""

import argparse
import csv
import io
from pathlib import Path
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--container', required=True)
    parser.add_argument('--user', required=True)
    parser.add_argument('--database', required=True)
    args = parser.parse_args()
    query = (Path(__file__).resolve().parents[1] / 'src/submissions.sql').read_text()
    # The production query has one integer bind. Use the fixture's literal id
    # so the exact same SQL can run through psql without extra Python packages.
    query = query.replace('$1', '7').rstrip().rstrip(';')
    fixtures = """
BEGIN;
CREATE TEMP TABLE contest (id int, start_time timestamptz, end_time timestamptz);
CREATE TEMP TABLE contest_user (contest_id int, user_id int);
CREATE TEMP TABLE contest_problem (contest_id int, problem_id int);
CREATE TEMP TABLE submission (
    id int, user_id int, problem_id int, contest_id int,
    contest_type text, status text, verdict text, created_at timestamptz
);
INSERT INTO contest VALUES
    (7, '2026-01-01 10:00Z', '2026-01-01 12:00Z'),
    (8, '2026-01-01 10:00Z', '2026-01-01 12:00Z');
INSERT INTO contest_user VALUES (7, 1), (7, 2), (8, 1);
INSERT INTO contest_problem VALUES (7, 1), (7, 2), (8, 1);
INSERT INTO submission
SELECT id, 1, 1, 7, 'codelink-qualifier', status, verdict,
       '2026-01-01 10:00Z'::timestamptz + id * interval '1 second'
FROM (VALUES
    (1, 'Queued', NULL), (2, 'Pending', NULL), (3, 'Compiling', NULL),
    (4, 'Running', NULL), (5, 'Judged', 'Accepted'),
    (6, 'CompilationError', NULL), (7, 'SystemError', 'SystemError'),
    (8, 'Judged', 'WrongAnswer'), (9, 'Judged', 'SystemError'),
    (10, 'Queued', NULL), (11, 'Queued', NULL), (12, 'Queued', NULL),
    (13, 'Queued', NULL), (14, 'Queued', NULL), (15, 'Running', NULL),
    (16, 'Queued', NULL)
) v(id, status, verdict);
UPDATE submission SET contest_id = 8 WHERE id = 10;
UPDATE submission SET user_id = 99 WHERE id = 11;
UPDATE submission SET problem_id = 99 WHERE id = 12;
UPDATE submission SET created_at = '2026-01-01 09:59Z' WHERE id = 13;
UPDATE submission SET created_at = '2026-01-01 12:01Z' WHERE id = 14;
UPDATE submission SET contest_type = 'icpc' WHERE id = 16;
"""
    completed = subprocess.run(
        ['docker', 'exec', '-i', args.container, 'psql', '-X', '-q', '--csv',
         '-v', 'ON_ERROR_STOP=1', '-U', args.user, '-d', args.database],
        input=fixtures + query + ';\nROLLBACK;\n', text=True, capture_output=True,
    )
    if completed.returncode:
        raise RuntimeError(completed.stderr.strip())
    rows = list(csv.DictReader(io.StringIO(completed.stdout)))
    observed = {int(r['submission_id']): (r['status'], r['verdict'] or None) for r in rows}
    # Every submission of the contest's window, participants and problems, as
    # its own row shows it. Pending and accepted are read from these columns
    # by `SubmissionRow` in `src/standings.rs`.
    expected = {
        1: ('Queued', None), 2: ('Pending', None), 3: ('Compiling', None),
        4: ('Running', None), 5: ('Judged', 'Accepted'),
        6: ('CompilationError', None), 7: ('SystemError', 'SystemError'),
        8: ('Judged', 'WrongAnswer'), 9: ('Judged', 'SystemError'),
        15: ('Running', None),
    }
    assert observed == expected, f'Expected {expected}, got {observed}'
    assert [int(r['submission_id']) for r in rows] == sorted(expected)
    assert all(int(r['submitted_at_us']) == int(r['submission_id']) * 1_000_000 for r in rows)
    print('PASS: production query reads each submission row in the contest window, once')


if __name__ == '__main__':
    main()

#!/usr/bin/env python3
"""Run the production standings query against PostgreSQL temporary fixtures.

Example with the local deployment:
  python3 plugins/codelink/tests/check_submissions_query.py \
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
    contest_type text, status text, created_at timestamptz
);
CREATE TEMP TABLE submission_judgement (
    id int, submission_id int, is_current bool, is_finalized bool,
    status text, verdict text
);
INSERT INTO contest VALUES
    (7, '2026-01-01 10:00Z', '2026-01-01 12:00Z'),
    (8, '2026-01-01 10:00Z', '2026-01-01 12:00Z');
INSERT INTO contest_user VALUES (7, 1), (7, 2), (8, 1);
INSERT INTO contest_problem VALUES (7, 1), (7, 2), (8, 1);
INSERT INTO submission
SELECT id, 1, 1, 7, 'codelink', status,
       '2026-01-01 10:00Z'::timestamptz + id * interval '1 second'
FROM (VALUES
    (1, 'Queued'), (2, 'Pending'), (3, 'Compiling'), (4, 'Running'),
    (5, 'Judged'), (6, 'CompilationError'), (7, 'SystemError'),
    (8, 'Judged'), (9, 'Judged'), (10, 'Queued'), (11, 'Queued'),
    (12, 'Queued'), (13, 'Queued'), (14, 'Queued'), (15, 'Running'),
    (16, 'Judged'), (17, 'SystemError'), (18, 'Queued')
) v(id, status);
UPDATE submission SET contest_id = 8 WHERE id = 10;
UPDATE submission SET user_id = 99 WHERE id = 11;
UPDATE submission SET problem_id = 99 WHERE id = 12;
UPDATE submission SET created_at = '2026-01-01 09:59Z' WHERE id = 13;
UPDATE submission SET created_at = '2026-01-01 12:01Z' WHERE id = 14;
INSERT INTO submission_judgement VALUES
    (3, 3, true, false, 'Compiling', NULL),
    (4, 4, true, false, 'Running', NULL),
    (5, 5, true, true, 'Judged', 'Accepted'),
    (6, 6, true, true, 'CompilationError', NULL),
    (8, 8, true, true, 'Judged', 'WrongAnswer'),
    (80, 8, false, true, 'Judged', 'Accepted'),
    (9, 9, true, true, 'Judged', 'Accepted'),
    (90, 9, false, false, 'Running', NULL),
    (17, 17, true, true, 'SystemError', 'SystemError'),
    (18, 18, false, false, 'Running', NULL);
"""
    completed = subprocess.run(
        ['docker', 'exec', '-i', args.container, 'psql', '-X', '-q', '--csv',
         '-v', 'ON_ERROR_STOP=1', '-U', args.user, '-d', args.database],
        input=fixtures + query + ';\nROLLBACK;\n', text=True, capture_output=True,
    )
    if completed.returncode:
        raise RuntimeError(completed.stderr.strip())
    rows = list(csv.DictReader(io.StringIO(completed.stdout)))
    observed = {int(r['submission_id']): (r['accepted'] == 't', r['pending'] == 't') for r in rows}
    expected = {
        1: (False, True), 2: (False, True), 3: (False, True), 4: (False, True),
        5: (True, False), 6: (False, False), 7: (False, False), 8: (False, False),
        9: (True, False), 15: (False, True), 16: (False, False),
        17: (False, False), 18: (False, True),
    }
    assert observed == expected, f'Expected {expected}, got {observed}'
    assert len(rows) == len(expected), 'A shadow judgement duplicated a submission'
    assert [int(r['submission_id']) for r in rows] == sorted(expected)
    assert all(int(r['submitted_at_us']) == int(r['submission_id']) * 1_000_000 for r in rows)
    print('PASS: production query covers queued/missing/current/shadow judgements and contest filters')


if __name__ == '__main__':
    main()

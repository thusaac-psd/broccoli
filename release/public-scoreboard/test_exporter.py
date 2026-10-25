import copy
from pathlib import Path
import json
import tempfile
import time
import unittest
from unittest.mock import patch
from urllib.error import HTTPError

from exporter import BroccoliClient, public_snapshot, write_snapshot


def sample():
    return {
        'phase': 'after', 'slots_per_problem': 2, 'solves_to_qualify': 3,
        'advancement_slots': 16, 'qualified_count': 1, 'pending_submissions': 0,
        'rows': [{
            'user_id': 7, 'username': 'oj_01', 'score': 3, 'credited': 2, 'accepted': 3,
            'qualification_verdict': 'qualified', 'qualified_at_seconds': 50,
            'password': 'private-password', 'email': 'private@example.test',
            'problems': {
                '1': {'status': 'credited', 'slot': 1, 'time_seconds': 20, 'submission_id': 99},
                '2': {'status': 'slots_full', 'slot': None, 'time_seconds': 40},
                '3': {'status': 'after_qualification', 'slot': None, 'time_seconds': 60},
            },
        }],
        'problems': [{
            'problem_id': n, 'label': label, 'points': 1, 'remaining': 1,
            'awards': [{'user_id': 7, 'username': 'oj_01', 'submission_id': 99,
                        'time_seconds': 20, 'password': 'nested-secret'}] if n == 1 else [],
        } for n, label in [(1, 'A'), (2, 'B'), (3, 'C')]],
        'advancement': {'direct_user_ids': [7], 'reserve_user_ids': [], 'unfilled_slots': 15},
        'token': 'private-token',
    }


CONTEST = {'id': 1, 'title': '预选赛', 'start_time': '2026-10-25T01:30:00Z',
           'end_time': '2026-10-25T03:30:00Z', 'description': 'private-description'}
PARTICIPANTS = [{'user_id': 7, 'username': 'oj_01', 'display_name': '选手甲'}]


class PublicSnapshotTests(unittest.TestCase):
    def test_projection_excludes_credentials_accounts_and_unrelated_fields(self):
        raw = sample()
        previous = copy.deepcopy(raw)
        data = public_snapshot(raw, CONTEST, PARTICIPANTS)
        text = json.dumps(data)
        for hidden in ['private-password', 'private-token', 'nested-secret',
                       'private-description', 'private@example.test', 'username', 'submission_id', 'oj_01']:
            self.assertNotIn(hidden, text)
        self.assertEqual(raw, previous)
        self.assertEqual(data['rows'][0]['display_name'], '选手甲')
        self.assertEqual(data['problems'][0]['awards'][0]['display_name'], '选手甲')

    def test_all_distinct_accepted_problems_survive_without_scoring_credit(self):
        row = public_snapshot(sample(), CONTEST, PARTICIPANTS)['rows'][0]
        self.assertEqual(row['accepted'], 3)
        self.assertEqual(row['credited'], 2)
        self.assertEqual([cell['status'] for cell in row['problems'].values()],
                         ['credited', 'slots_full', 'after_qualification'])
        self.assertEqual(row['problems']['3']['time_seconds'], 60)

    def test_invalid_upstream_does_not_become_a_successful_snapshot(self):
        for key, value in [('accepted', -1), ('score', True)]:
            raw = sample()
            raw['rows'][0][key] = value
            with self.assertRaises(ValueError):
                public_snapshot(raw, CONTEST, PARTICIPANTS)

    def test_atomic_write_preserves_last_good_snapshot_on_serialization_error(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'standings.json'
            write_snapshot(path, {'rows': [1]})
            with self.assertRaises(ValueError):
                write_snapshot(path, {'score': float('nan')})
            self.assertEqual(json.loads(path.read_text()), {'rows': [1]})
            self.assertEqual(path.stat().st_mode & 0o777, 0o644)
            self.assertEqual([p.name for p in path.parent.iterdir()], ['standings.json'])

    def test_client_reauthenticates_on_401_and_accepts_existing_token_shape(self):
        client = BroccoliClient('http://127.0.0.1:3000', 'staff', 'secret', 1)
        with patch.object(client, '_request', side_effect=[
            {'token': 'first'}, HTTPError('unused', 401, 'expired', {}, None),
            {'token': 'second'}, {'rows': []},
        ]) as request:
            self.assertEqual(client.get('/fixed'), {'rows': []})
        self.assertEqual(client.token, 'second')
        self.assertEqual([call.args[0] for call in request.call_args_list],
                         ['/api/v1/auth/login', '/fixed', '/api/v1/auth/login', '/fixed'])

    def test_client_refreshes_before_expiry_even_if_old_server_hides_auth_errors(self):
        client = BroccoliClient('http://127.0.0.1:3000', 'staff', 'secret', 1)
        client.token, client.token_expires_at = 'expired', time.time() - 1
        with patch.object(client, '_request', side_effect=[{'access_token': 'fresh'}, {'rows': []}]) as request:
            client.get('/fixed')
        self.assertEqual(request.call_args_list[0].args[0], '/api/v1/auth/login')
        self.assertEqual(client.token, 'fresh')

    def test_client_reads_only_fixed_contest_and_its_participants(self):
        client = BroccoliClient('http://127.0.0.1:3000', 'staff', 'secret', 1)
        with patch.object(client, 'get', side_effect=[sample(),
                          {**CONTEST, 'contest_type': 'codelink-qualifier'}, {'data': PARTICIPANTS}]) as request:
            result = client.snapshot()
        self.assertEqual(result['contest_id'], 1)
        self.assertEqual([call.args[0] for call in request.call_args_list], [
            '/api/v1/p/codelink-qualifier/api/plugins/codelink-qualifier/contests/1/standings',
            '/api/v1/contests/1', '/api/v1/contests/1/participants?page=1&page_size=100',
        ])


if __name__ == '__main__':
    unittest.main()

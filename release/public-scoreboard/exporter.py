"""Export one contest through authenticated GETs; publish only scoreboard fields."""

import base64
import json
import logging
import os
from pathlib import Path
import signal
import tempfile
import threading
import time
from datetime import datetime, timezone
from urllib.error import HTTPError
from urllib.request import Request, urlopen


def nonnegative(value):
    if type(value) is not int or value < 0:
        raise ValueError('Invalid scoreboard number')
    return value


def display_name(record, names):
    value = record.get('display_name') or names.get(record['user_id'])
    return value.strip() if isinstance(value, str) and value.strip() else f"选手 {record['user_id']}"


def public_snapshot(raw, contest, participants):
    """Project explicitly, including nested cells; never publish upstream records."""
    names = {p['user_id']: p.get('display_name') for p in participants}
    rows = []
    allowed_statuses = {'credited', 'slots_full', 'after_qualification'}
    problem_ids = {str(p['problem_id']) for p in raw['problems']}
    for row in raw['rows']:
        cells = {}
        for key, cell in row['problems'].items():
            if str(key) not in problem_ids or cell['status'] not in allowed_statuses:
                raise ValueError('Invalid scoreboard cell')
            cells[str(key)] = {
                'status': cell['status'],
                'time_seconds': nonnegative(cell['time_seconds']),
                'slot': nonnegative(cell['slot']) if cell.get('slot') is not None else None,
            }
        rows.append({
            'user_id': nonnegative(row['user_id']),
            'display_name': display_name(row, names),
            'score': nonnegative(row['score']),
            'credited': nonnegative(row['credited']),
            'accepted': nonnegative(row['accepted']),
            'qualification_verdict': row.get('qualification_verdict')
            if row.get('qualification_verdict') in {'qualified', 'pending'} else None,
            'qualified_at_seconds': nonnegative(row['qualified_at_seconds'])
            if row.get('qualified_at_seconds') is not None else None,
            'problems': cells,
        })
    row_ids = {row['user_id'] for row in rows}
    problems = []
    for problem in raw['problems']:
        awards = []
        for award in problem['awards']:
            if award['user_id'] not in row_ids:
                raise ValueError('Unknown scoring-slot owner')
            awards.append({
                'user_id': award['user_id'],
                'display_name': display_name(award, names),
                'time_seconds': nonnegative(award['time_seconds']),
            })
        problems.append({
            'problem_id': nonnegative(problem['problem_id']),
            'label': str(problem['label']),
            'points': nonnegative(problem['points']),
            'remaining': nonnegative(problem['remaining']),
            'awards': awards,
        })
    phase = raw['phase']
    if phase not in {'before', 'during', 'after'}:
        raise ValueError('Invalid contest phase')
    advancement = None
    if raw.get('advancement') is not None:
        advance = raw['advancement']
        advancement = {
            'direct_user_ids': [uid for uid in advance['direct_user_ids'] if uid in row_ids],
            'reserve_user_ids': [uid for uid in advance['reserve_user_ids'] if uid in row_ids],
            'unfilled_slots': nonnegative(advance['unfilled_slots']),
        }
    output = {
        'contest_id': nonnegative(contest['id']),
        'title': str(contest['title']),
        'start_time': contest['start_time'],
        'end_time': contest['end_time'],
        'phase': phase,
        'updated_at': datetime.now(timezone.utc).isoformat(timespec='seconds'),
        'rows': rows,
        'problems': problems,
        'advancement': advancement,
    }
    for key in ['slots_per_problem', 'solves_to_qualify', 'advancement_slots',
                'qualified_count', 'pending_submissions']:
        output[key] = nonnegative(raw[key])
    return output


class BroccoliClient:
    def __init__(self, base_url, username, password, contest_id):
        self.base = base_url.rstrip('/')
        self.username = username
        self.password = password
        self.contest_id = contest_id
        self.token = None
        self.token_expires_at = 0
        self.metadata = None
        self.participants = None
        self.metadata_at = 0

    def _request(self, path, payload=None, authenticated=True):
        headers = {'Accept': 'application/json'}
        if authenticated and self.token:
            headers['Authorization'] = 'Bearer ' + self.token
        data = None
        if payload is not None:
            data = json.dumps(payload).encode()
            headers['Content-Type'] = 'application/json'
        with urlopen(Request(self.base + path, data=data, headers=headers), timeout=8) as response:
            body = response.read(8 * 1024 * 1024 + 1)
        if len(body) > 8 * 1024 * 1024:
            raise ValueError('Upstream response exceeds size limit')
        return json.loads(body)

    def login(self):
        auth = self._request('/api/v1/auth/login', {
            'username': self.username, 'password': self.password,
        }, authenticated=False)
        self.token = auth.get('token') or auth.get('access_token')
        if not isinstance(self.token, str) or not self.token:
            raise ValueError('Invalid login response')
        # Expiry is only a refresh hint. Broccoli still verifies every token.
        try:
            part = self.token.split('.')[1]
            claims = json.loads(base64.urlsafe_b64decode(part + '=' * (-len(part) % 4)))
            self.token_expires_at = float(claims['exp'])
        except (ValueError, KeyError, IndexError, TypeError):
            self.token_expires_at = time.time() + 240

    def get(self, path):
        if self.token is None or self.token_expires_at < time.time() + 30:
            self.login()
        try:
            return self._request(path)
        except HTTPError as error:
            if error.code != 401:
                raise
            self.login()
            return self._request(path)

    def snapshot(self):
        cid = self.contest_id
        raw = self.get(f'/api/v1/p/codelink-qualifier/api/plugins/codelink-qualifier/contests/{cid}/standings')
        if self.metadata is None or time.monotonic() - self.metadata_at > 300:
            contest = self.get(f'/api/v1/contests/{cid}')
            if contest.get('id') != cid or contest.get('contest_type') != 'codelink-qualifier':
                raise ValueError('Unexpected contest')
            participants = []
            for page in range(1, 101):
                result = self.get(f'/api/v1/contests/{cid}/participants?page={page}&page_size=100')
                batch = result if isinstance(result, list) else result['data']
                participants.extend(batch)
                if len(batch) < 100:
                    break
            else:
                raise ValueError('Too many participant pages')
            self.metadata, self.participants = contest, participants
            self.metadata_at = time.monotonic()
        return public_snapshot(raw, self.metadata, self.participants)


def write_snapshot(path, data):
    """Readers see either the last successful snapshot or the complete new one."""
    encoded = json.dumps(data, ensure_ascii=False, separators=(',', ':'), allow_nan=False).encode()
    with tempfile.NamedTemporaryFile(dir=path.parent, prefix='.standings-', delete=False) as stream:
        temp = Path(stream.name)
        try:
            stream.write(encoded)
            stream.flush()
            os.fchmod(stream.fileno(), 0o644)
            os.replace(temp, path)
        finally:
            temp.unlink(missing_ok=True)


def main():
    logging.basicConfig(level=logging.INFO, format='%(levelname)s %(message)s')
    cid = int(os.environ.get('CONTEST_ID', '1'))
    interval = max(1, int(os.environ.get('REFRESH_SECONDS', '5')))
    client = BroccoliClient(os.environ.get('BROCCOLI_BASE_URL', 'http://127.0.0.1:3000'),
                            os.environ['BROCCOLI_USERNAME'], os.environ['BROCCOLI_PASSWORD'], cid)
    directory = Path(os.environ.get('STATE_DIRECTORY', '/var/lib/codelink-public-scoreboard'))
    directory.mkdir(parents=True, exist_ok=True)
    stop = threading.Event()
    for sig in [signal.SIGTERM, signal.SIGINT]:
        signal.signal(sig, lambda *_: stop.set())
    last_counts = None
    failed = False
    while not stop.is_set():
        started = time.monotonic()
        try:
            data = client.snapshot()
            write_snapshot(directory / 'standings.json', data)
            counts = len(data['rows']), len(data['problems'])
            if counts != last_counts or failed:
                logging.info('Published contest %s: %s contestants, %s problems', cid, *counts)
            last_counts, failed = counts, False
        except Exception as error:
            # Never log upstream bodies, login payloads, tokens, or credentials.
            logging.error('Snapshot unavailable (%s); retaining last successful snapshot', type(error).__name__)
            failed = True
        stop.wait(max(0.1, interval - (time.monotonic() - started)))


if __name__ == '__main__':
    main()

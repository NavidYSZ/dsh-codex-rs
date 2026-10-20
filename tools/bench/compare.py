"""Wall time and peak memory of dsh-codex vs. the official dsh, both against
the instant offline mock (so only the harness itself is measured).

    python3 tools/bench/compare.py [RUNS]

Needs `dsh` on PATH and a release build (`cargo build --release -p dsh-codex`).
Starts two mocks on 18180 (no tools) and 18185 (five bash calls)."""
import os, statistics, subprocess, sys, tempfile, threading, time

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BIN = os.path.join(ROOT, 'target/release/dsh-codex')
MOCK = os.path.join(ROOT, 'tools/dev/mock_deepseek.py')


def tree_rss_kb(root):
    total, todo, seen = 0, [root], set()
    while todo:
        pid = todo.pop()
        if pid in seen:
            continue
        seen.add(pid)
        try:
            with open(f'/proc/{pid}/status') as f:
                total += next(int(l.split()[1]) for l in f if l.startswith('VmRSS:'))
            with open(f'/proc/{pid}/task/{pid}/children') as f:
                todo += [int(x) for x in f.read().split()]
        except (OSError, StopIteration):
            pass
    return total


def run(cmd, port, cwd, home):
    env = dict(os.environ, DEEPSEEK_API_KEY='sk-mock', DEEPSEEK_BASE_URL=f'http://127.0.0.1:{port}/anthropic',
               DSHX_HOME=home)
    t0 = time.perf_counter()
    p = subprocess.Popen(cmd, cwd=cwd, env=env, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    peak = 0

    def sample():
        nonlocal peak
        while p.poll() is None:
            peak = max(peak, tree_rss_kb(p.pid))
            time.sleep(0.002)

    th = threading.Thread(target=sample)
    th.start()
    out, _ = p.communicate()
    th.join()
    return (time.perf_counter() - t0) * 1000, peak / 1024, out.decode().strip()


def main():
    runs = int(sys.argv[1]) if len(sys.argv) > 1 else 7
    mocks = [subprocess.Popen([sys.executable, MOCK, str(port), str(n)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
             for port, n in ((18180, 0), (18185, 5))]
    time.sleep(0.5)
    try:
        ws = tempfile.mkdtemp()
        with tempfile.TemporaryDirectory() as home:
            for label, port in (('answer, no tools', 18180), ('five bash calls', 18185)):
                for name, cmd in (('dsh 0.2 (Node)', ['dsh', '--profile', 'headless', 'task']), ('dsh-codex (Rust)', [BIN, 'task'])):
                    res = [run(cmd, port, ws, home) for _ in range(runs)]
                    walls = [r[0] for r in res]
                    print(f'{label:17s} {name:17s} median {statistics.median(walls):6.0f} ms  '
                          f'min {min(walls):5.0f} ms  peak RSS {max(r[1] for r in res):6.1f} MB  answer={res[-1][2]!r}')
    finally:
        for m in mocks:
            m.kill()
        # The official dsh keeps its sessions under ~/.dsh; remove the benchmark's.
        import shutil
        shutil.rmtree(os.path.expanduser('~/.dsh/sessions/-' + ws.replace('/', '-') + '--'), ignore_errors=True)


if __name__ == '__main__':
    main()

// Results of the persistent `bash` of Minimal chats (dsh-tool-bash-persistent):
// one combined output, then a status line instead of `[exit code: N]`.
import { t } from './i18n.js';

const RESET = 'The persistent bash shell was reset;';
const FINISHED = /(?:^|\n)\[Command finished with exit code (-?\d+)\]$/;
const EXITED = /(?:^|\n)\[shell (?:exited: code (-?\d+)|killed by signal: (\w+)|exited)\]\nThe persistent bash shell was reset;[^\n]*$/;
const TIMED_OUT = /^Your command timed out after (\d+) seconds or experienced an OOM error\. Below is partial output:\n([\s\S]*?)\n?\[Command timed out or OOM\]\nThe persistent bash shell was reset;[^\n]*$/;
const RESTARTED = /^\[The persistent bash shell was restarted for the [\w-]+ sandbox mode;[^\n]*\]\n/;

/**
 * `{stdout, stderr, exit, note, noteError}` like `splitBashOutput`, or null
 * when the text is not a persistent-shell result.
 */
export function splitShellOutput(text) {
  if (!text || !(text.includes('[Command finished with exit code') || text.includes(RESET))) return null;
  let body = text;
  let note = null;
  const restarted = body.match(RESTARTED);
  if (restarted) {
    body = body.slice(restarted[0].length);
    note = t.shellRestarted;
  }
  const timedOut = body.match(TIMED_OUT);
  if (timedOut) {
    return { stdout: timedOut[2], stderr: '', exit: null, note: t.shellTimedOut(Number(timedOut[1])), noteError: true };
  }
  const exited = body.match(EXITED);
  if (exited) {
    const exit = exited[1] != null ? Number(exited[1]) : exited[2] ?? null;
    return { stdout: body.slice(0, exited.index), stderr: '', exit, note: t.shellReset, noteError: true };
  }
  const finished = body.match(FINISHED);
  if (!finished) return null;
  return { stdout: body.slice(0, finished.index), stderr: '', exit: Number(finished[1]), note, noteError: false };
}

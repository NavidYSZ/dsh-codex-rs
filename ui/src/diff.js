// Line diff for the edit/write cards. dsh stores one {oldText, newText}
// pair per hunk (3 context lines each side), so inputs are small and a
// plain LCS table is enough.

const MAX_CELLS = 400_000;

/** → [{ op: ' ' | '-' | '+', text }] */
export function diffLines(oldText, newText) {
  const a = oldText == null ? [] : oldText.split('\n');
  const b = newText == null ? [] : newText.split('\n');
  if (!a.length) return b.map((text) => ({ op: '+', text }));
  if (!b.length) return a.map((text) => ({ op: '-', text }));
  // Trim the common head and tail so the table only covers the change.
  let head = 0;
  while (head < a.length && head < b.length && a[head] === b[head]) head++;
  let tail = 0;
  while (tail < a.length - head && tail < b.length - head && a[a.length - 1 - tail] === b[b.length - 1 - tail]) tail++;
  const A = a.slice(head, a.length - tail);
  const B = b.slice(head, b.length - tail);
  const mid = [];
  if ((A.length + 1) * (B.length + 1) > MAX_CELLS) {
    for (const text of A) mid.push({ op: '-', text });
    for (const text of B) mid.push({ op: '+', text });
  } else {
    const n = A.length, m = B.length;
    const w = m + 1;
    const L = new Uint32Array((n + 1) * w);
    for (let i = n - 1; i >= 0; i--) {
      for (let j = m - 1; j >= 0; j--) {
        L[i * w + j] = A[i] === B[j] ? L[(i + 1) * w + j + 1] + 1 : Math.max(L[(i + 1) * w + j], L[i * w + j + 1]);
      }
    }
    let i = 0, j = 0;
    while (i < n && j < m) {
      if (A[i] === B[j]) { mid.push({ op: ' ', text: A[i] }); i++; j++; }
      else if (L[(i + 1) * w + j] >= L[i * w + j + 1]) mid.push({ op: '-', text: A[i++] });
      else mid.push({ op: '+', text: B[j++] });
    }
    while (i < n) mid.push({ op: '-', text: A[i++] });
    while (j < m) mid.push({ op: '+', text: B[j++] });
  }
  return [
    ...a.slice(0, head).map((text) => ({ op: ' ', text })),
    ...mid,
    ...a.slice(a.length - tail).map((text) => ({ op: ' ', text })),
  ];
}

export function countChanges(rows) {
  let add = 0, del = 0;
  for (const r of rows) {
    if (r.op === '+') add++;
    else if (r.op === '-') del++;
  }
  return { add, del };
}

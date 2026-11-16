#!/usr/bin/env python3
"""Run the book's examples against a norn binary.

    python3 docs/book/check.py [--norn PATH] [--keep DIR] [CHAPTER.md ...]

With no chapters given, every numbered chapter in docs/book runs. Each
chapter's ```clojure blocks run in order, in one fresh norn process, as one
script, so a later block can use what an earlier one defined. Top-level
forms are checked by the comment that follows them:

    (+ 1 2) ; => 3              the value, compared with = after reading the
                                text, or else by its printed form
    (/ 1 0) ; !! ArithmeticError  the form signals a condition of this class
    (println "hi")
    ; prints: hi                what the form prints (consecutive lines join)

An expected value may continue onto following comment lines while its
brackets are open. A form without one of these comments runs unchecked, and
an error in it ends the chapter's run. Put <!-- skip --> on the line before
a fence for a block that shouldn't run (it needs the network, a terminal,
a file it doesn't create...). Other fences (sh, edn, text) never run.
"""

import argparse
import os
import re
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))

PRELUDE = r'''
(require 'norn.string 'norn.sys)
(def book-checks (atom 0))
(def book-failures (atom []))

(defn- book-norm [s]
  (-> s (norn.string/replace "," " ") (norn.string/replace #"\s+" " ") norn.string/trim))

(defn- book-fail [loc & lines]
  (swap! book-failures conj loc)
  (println (str "FAIL " loc))
  (doseq [l lines] (println (str "  " l))))

(defn- book-run [thunk]
  ;; Serious conditions only: a signal or warning that no one handles
  ;; returns normally, as it would in a program.
  (handler-case {:ok (thunk)} (SeriousCondition [e] {:err e})))

(defn- book-run-any [thunk]
  (handler-case {:ok (thunk)} (Condition [e] {:err e})))

(defn- book-describe [e]
  (str (class-name (class-of e)) ": " (condition-message e)))

(defn book-value [loc thunk src]
  (swap! book-checks inc)
  (let [r (book-run thunk)]
    (if (contains? r :err)
      (book-fail loc (str "expected: " src) (str "raised:   " (book-describe (:err r))))
      (let [v (:ok r)
            exp (handler-case {:ok (read-string src)} (Condition [_] nil))]
        (when-not (or (and exp (= v (:ok exp)))
                      (= (book-norm (pr-str v)) (book-norm src)))
          (book-fail loc (str "expected: " src) (str "got:      " (pr-str v))))))))

(defn book-raises [loc thunk cls]
  (swap! book-checks inc)
  (let [r (book-run-any thunk)]
    (cond
      (contains? r :ok)
      (book-fail loc (str "expected a " (class-name cls)) (str "got:      " (pr-str (:ok r))))
      (not (instance? cls (:err r)))
      (book-fail loc (str "expected a " (class-name cls)) (str "raised:   " (book-describe (:err r)))))))

(defn book-prints [loc thunk expected]
  (swap! book-checks inc)
  (let [out (atom nil)
        r (book-run (fn [] (reset! out (with-out-str (thunk)))))]
    (cond
      (contains? r :err)
      (book-fail loc (str "expected output: " (pr-str expected)) (str "raised: " (book-describe (:err r))))
      (not= (norn.string/trimr @out) (norn.string/trimr expected))
      (book-fail loc (str "expected output: " (pr-str expected)) (str "printed:         " (pr-str @out))))))

(defn book-summary []
  (println (str "BOOK " @book-checks " checks, " (count @book-failures) " failed"))
  (norn.sys/exit (if (empty? @book-failures) 0 1)))
'''


def blocks(path):
    """Yield (first_line_number, text) for each runnable clojure block."""
    lines = open(path, encoding="utf-8").read().split("\n")
    i = 0
    prev = ""
    while i < len(lines):
        m = re.match(r"^(\s*)```+\s*(\S*)", lines[i])
        if m:
            fence = lines[i].strip()
            ticks = re.match(r"`+", fence).group(0)
            lang = m.group(2)
            start = i + 1
            j = start
            while j < len(lines) and not lines[j].strip().startswith(ticks):
                j += 1
            skip = "<!-- skip -->" in prev
            if lang in ("clojure", "norn") and not skip:
                yield start + 1, "\n".join(lines[start:j])
            i = j + 1
            prev = ""
            continue
        if lines[i].strip():
            prev = lines[i]
        i += 1


def scan(text):
    """Split a block into top-level items.

    Returns a list of ("form", start, end) and ("comment", start, end, text)
    in source order, where start and end are offsets into text."""
    items = []
    i, n = 0, len(text)
    depth = 0
    form_start = None

    def skip_string(k):
        k += 1
        while k < n and text[k] != '"':
            k += 2 if text[k] == "\\" else 1
        return k + 1

    while i < n:
        c = text[i]
        if c == ";":
            j = text.find("\n", i)
            j = n if j < 0 else j
            if depth == 0 and form_start is None:
                items.append(("comment", i, j, text[i:j]))
            i = j
            continue
        if c == "#" and i + 1 < n and text[i + 1] == "|":
            level, k = 1, i + 2
            while k < n and level:
                if text.startswith("|#", k):
                    level -= 1
                    k += 2
                elif text.startswith("#|", k):
                    level += 1
                    k += 2
                else:
                    k += 1
            i = k
            continue
        if c.isspace() or c == ",":
            if depth == 0 and form_start is not None:
                items.append(("form", form_start, i))
                form_start = None
            i += 1
            continue
        if form_start is None:
            form_start = i
        if c == '"':
            i = skip_string(i)
        elif c == "#" and i + 1 < n and text[i + 1] == '"':
            i = skip_string(i + 1)
        elif c == "\\":
            # A character literal: \a, \newline, \(, \u00e9.
            i += 2
            while i < n and (text[i].isalnum()):
                i += 1
        elif c in "([{":
            depth += 1
            i += 1
        elif c in ")]}":
            depth -= 1
            i += 1
            if depth == 0:
                items.append(("form", form_start, i))
                form_start = None
        else:
            i += 1
    if form_start is not None:
        items.append(("form", form_start, n))
    return merge_prefixes(text, items)


def merge_prefixes(text, items):
    """Join reader prefixes (', #_, ^meta ...) to the form after them."""
    out = []
    pending = None
    for it in items:
        if it[0] != "form":
            out.append(it)
            continue
        s = text[it[1]:it[2]]
        start = pending if pending is not None else it[1]
        if re.fullmatch(r"(?:'|`|~@?|@|#'|#_|#;|#|\^)+", s) or s.startswith("^"):
            pending = start  # a bare prefix, or metadata waiting for its form
            continue
        out.append(("form", start, it[2]))
        pending = None
    return out


def balanced(s):
    depth = 0
    in_str = False
    k = 0
    while k < len(s):
        ch = s[k]
        if in_str:
            if ch == "\\":
                k += 1
            elif ch == '"':
                in_str = False
        elif ch == '"':
            in_str = True
        elif ch in "([{":
            depth += 1
        elif ch in ")]}":
            depth -= 1
        k += 1
    return depth <= 0 and not in_str


def norn_str(s):
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n") + '"'


def translate(path, first_line, text):
    """The block as script text, with checks wrapped around checked forms."""
    items = scan(text)
    line_of = lambda off: first_line + text.count("\n", 0, off)
    rel = os.path.relpath(path)
    out = []
    k = 0
    while k < len(items):
        it = items[k]
        if it[0] != "form":
            k += 1
            continue
        form = text[it[1]:it[2]]
        loc = f"{rel}:{line_of(it[1])}"
        # Gather the comments right after the form.
        j = k + 1
        comments = []
        while j < len(items) and items[j][0] == "comment":
            comments.append(items[j])
            j += 1
        check = None
        if comments:
            body = comments[0][3]
            m = re.match(r";+\s*=>\s?(.*)", body)
            if m:
                exp = m.group(1).strip()
                used = 1
                while not balanced(exp) and used < len(comments):
                    exp += " " + re.sub(r"^;+\s*", "", comments[used][3])
                    used += 1
                check = ("value", exp)
            m = re.match(r";+\s*!!\s*(\S+)", body)
            if m:
                check = ("raises", m.group(1))
            if re.match(r";+\s*prints:", body):
                outs = []
                for c in comments:
                    m = re.match(r";+\s*prints:\s?(.*)", c[3])
                    if not m:
                        break
                    outs.append(m.group(1))
                check = ("prints", "\n".join(outs))
        mark = f";; {loc}"
        if check is None:
            out.append(f"{mark}\n{form}")
        elif check[0] == "value":
            out.append(f"{mark}\n(user/book-value {norn_str(loc)} (fn [] {form}\n) {norn_str(check[1])})")
        elif check[0] == "raises":
            out.append(f"{mark}\n(user/book-raises {norn_str(loc)} (fn [] {form}\n) {check[1]})")
        else:
            out.append(f"{mark}\n(user/book-prints {norn_str(loc)} (fn [] {form}\n) {norn_str(check[1])})")
        k = j
    return "\n".join(out)


def chapter_script(path):
    parts = [PRELUDE]
    for first_line, text in blocks(path):
        parts.append(translate(path, first_line, text))
    parts.append("(in-ns 'user)\n(user/book-summary)")
    return "\n\n".join(parts) + "\n"


def run(norn, path, keep):
    script = chapter_script(path)
    with tempfile.TemporaryDirectory() as tmp:
        name = os.path.splitext(os.path.basename(path))[0] + ".norn"
        spath = os.path.join(keep or tmp, name)
        with open(spath, "w", encoding="utf-8") as f:
            f.write(script)
        workdir = tempfile.mkdtemp(dir=tmp)
        try:
            p = subprocess.run([norn, spath], cwd=workdir, capture_output=True,
                               text=True, timeout=180,
                               env={**os.environ, "NORN_UI_HEADLESS": "1"})
        except subprocess.TimeoutExpired:
            return False, "TIMEOUT after 180 s"
    out = p.stdout + p.stderr
    # Point script line numbers back at the chapter.
    lines = script.split("\n")

    def where(m):
        n = int(m.group(1))
        for k in range(min(n, len(lines)) - 1, -1, -1):
            if lines[k].startswith(";; "):
                return f"{lines[k][3:]} (+{n - k - 2})"
        return m.group(0)
    out = re.sub(re.escape(name) + r":(\d+)", where, out)
    ok = p.returncode == 0 and "BOOK " in out
    return ok, out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--norn", default=os.environ.get("NORN", os.path.join(HERE, "../../target/release/norn")))
    ap.add_argument("--keep", help="write the generated scripts to this folder")
    ap.add_argument("--verbose", "-v", action="store_true")
    ap.add_argument("chapters", nargs="*")
    a = ap.parse_args()
    chapters = a.chapters or sorted(
        os.path.join(HERE, f) for f in os.listdir(HERE) if re.match(r"\d\d-.*\.md$", f))
    bad = 0
    for ch in chapters:
        ok, out = run(os.path.abspath(a.norn), ch, a.keep)
        summary = next((l for l in out.splitlines() if l.startswith("BOOK ")), "no summary (the run ended early)")
        print(f"{'ok  ' if ok else 'FAIL'} {os.path.relpath(ch)}: {summary[5:] if summary.startswith('BOOK ') else summary}")
        if not ok or a.verbose:
            bad += 0 if ok else 1
            for l in out.splitlines():
                if not l.startswith("BOOK "):
                    print("    " + l)
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()

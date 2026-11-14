// Minimal Playwright-like selector engine, evaluated inside the page.
//
// Called as `(steps, op, arg) => result` where `steps` is the locator chain:
//   [{ t: "sel", v: "<selector>" } | { t: "nth", v: <index> }, ...]
//
// Supported selector syntaxes (the ones linkedin_scraper relies on):
//   - CSS, including comma lists and relative selectors starting with ">"
//   - Playwright pseudo-class  :has-text("...")  (case-insensitive substring)
//   - text="..."  (exact, whitespace-normalized)  and  text=...  (substring, case-insensitive)
//   - xpath=...   (and bare expressions starting with "/" or "..")
(steps, op, arg) => {
  const norm = (s) => (s || "").replace(/\s+/g, " ").trim();

  const docOrder = (els) => {
    const uniq = [...new Set(els)];
    uniq.sort((a, b) => {
      if (a === b) return 0;
      return a.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING ? -1 : 1;
    });
    return uniq;
  };

  // Split a selector on top-level commas (ignoring commas inside quotes, (), []).
  const splitTopLevel = (sel) => {
    const parts = [];
    let depth = 0, quote = null, cur = "";
    for (let i = 0; i < sel.length; i++) {
      const c = sel[i];
      if (quote) {
        if (c === "\\") { cur += c + (sel[i + 1] || ""); i++; continue; }
        if (c === quote) quote = null;
      } else if (c === '"' || c === "'") {
        quote = c;
      } else if (c === "(" || c === "[") {
        depth++;
      } else if (c === ")" || c === "]") {
        depth--;
      } else if (c === "," && depth === 0) {
        parts.push(cur.trim()); cur = ""; continue;
      }
      cur += c;
    }
    if (cur.trim()) parts.push(cur.trim());
    return parts;
  };

  const unquote = (s) => {
    s = s.trim();
    if ((s.startsWith('"') && s.endsWith('"')) || (s.startsWith("'") && s.endsWith("'"))) {
      return { text: s.slice(1, -1), exact: true };
    }
    return { text: s, exact: false };
  };

  const queryXPath = (root, expr) => {
    if (root !== document && expr.startsWith("/")) expr = "." + expr;
    const res = document.evaluate(expr, root, null, XPathResult.ORDERED_NODE_SNAPSHOT_TYPE, null);
    const out = [];
    for (let i = 0; i < res.snapshotLength; i++) {
      const n = res.snapshotItem(i);
      if (n && n.nodeType === Node.ELEMENT_NODE) out.push(n);
    }
    return out;
  };

  const queryText = (root, raw) => {
    const { text, exact } = unquote(raw);
    const want = exact ? norm(text) : norm(text).toLowerCase();
    const all = Array.from(root.querySelectorAll("*"));
    const matches = all.filter((e) => {
      const t = norm(e.textContent);
      return exact ? t === want : t.toLowerCase().includes(want);
    });
    // Keep the deepest matching elements only (like Playwright).
    const set = new Set(matches);
    return matches.filter((e) => !Array.from(e.querySelectorAll("*")).some((d) => set.has(d)));
  };

  const queryCss = (root, part) => {
    const texts = [];
    let base = part.replace(/:has-text\(\s*(["'])((?:\\.|(?!\1).)*)\1\s*\)/g, (_m, _q, t) => {
      texts.push(t);
      return "";
    }).trim();
    if (base === "") base = "*";
    if (base.startsWith(">")) base = ":scope " + base;
    let els = Array.from(root.querySelectorAll(base));
    for (const t of texts) {
      const needle = norm(t).toLowerCase();
      els = els.filter((e) => norm(e.textContent).toLowerCase().includes(needle));
    }
    return els;
  };

  const query = (root, sel) => {
    sel = sel.trim();
    if (sel.startsWith("xpath=")) return queryXPath(root, sel.slice(6));
    if (sel.startsWith("/") || sel.startsWith("..")) return queryXPath(root, sel);
    if (sel.startsWith("text=")) return queryText(root, sel.slice(5));
    if (sel.startsWith("css=")) sel = sel.slice(4);
    const out = [];
    for (const part of splitTopLevel(sel)) out.push(...queryCss(root, part));
    return out;
  };

  let roots = [document];
  for (const st of steps) {
    if (st.t === "nth") {
      const i = st.v < 0 ? roots.length + st.v : st.v;
      roots = i >= 0 && i < roots.length ? [roots[i]] : [];
      continue;
    }
    const out = [];
    for (const r of roots) out.push(...query(r, st.v));
    roots = docOrder(out);
  }

  const isVisible = (el) => {
    if (!el || !el.isConnected) return false;
    const style = window.getComputedStyle(el);
    if (style.visibility === "hidden" || style.visibility === "collapse") return false;
    const rect = el.getBoundingClientRect();
    return rect.width > 0 && rect.height > 0;
  };

  const el = roots[0];
  const found = !!el;

  switch (op) {
    case "count":
      return roots.length;
    case "state": {
      // arg: attached | visible | hidden | detached
      if (arg === "attached") return found;
      if (arg === "detached") return !found;
      if (arg === "hidden") return !found || !isVisible(el);
      return found && isVisible(el);
    }
    case "isVisible":
      return found && isVisible(el);
    case "textContent":
      return found ? { found, v: el.textContent } : { found };
    case "innerText":
      return found ? { found, v: el.innerText } : { found };
    case "getAttribute":
      return found ? { found, v: el.getAttribute(arg) } : { found };
    case "allInnerTexts":
      return roots.map((e) => e.innerText);
    case "allTextContents":
      return roots.map((e) => e.textContent);
    case "evaluateAll":
      // arg is a function taking the element array (composed on the Rust side).
      return arg(roots);
    case "scrollIntoView":
      if (found) el.scrollIntoView({ block: "center", inline: "center" });
      return found;
    case "clickPoint": {
      if (!found) return { found };
      el.scrollIntoView({ block: "center", inline: "center" });
      if (!isVisible(el)) return { found, visible: false };
      const r = el.getBoundingClientRect();
      return { found, visible: true, x: r.left + r.width / 2, y: r.top + r.height / 2 };
    }
    case "focusClear": {
      if (!found) return { found };
      if (!isVisible(el)) return { found, visible: false };
      el.focus();
      if ("value" in el) {
        el.value = "";
        el.dispatchEvent(new Event("input", { bubbles: true }));
      } else if (el.isContentEditable) {
        el.textContent = "";
      }
      return { found, visible: true };
    }
    default:
      throw new Error("unknown locator op: " + op);
  }
}

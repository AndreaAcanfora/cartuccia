// Syntax highlighting for the editor: a <pre> under a transparent <textarea>.
// Pixel art between [[ and ]] is drawn in its four shades.

const KEYWORDS = new Set([
  "function", "end", "if", "then", "elseif", "else", "while", "do", "for", "local", "var",
  "const", "gfx", "return", "break", "and", "or", "not", "true", "false",
]);
const TYPES = new Set(["u8", "i8", "u16", "i16", "bool"]);
const BUTTONS = new Set(["A", "B", "SELECT", "START", "RIGHT", "LEFT", "UP", "DOWN"]);
export const BUILTINS = new Set([
  "btn", "btnp", "spr", "hide", "tile", "gettile", "text", "num", "cls", "rnd", "beep",
  "noise", "scroll", "frames", "vsync", "debug", "stop", "abs", "min", "max",
]);

const escape = (s) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

function art(block) {
  return block.replace(/[.0123#]/g, (c) => {
    const shade = c === "." || c === "0" ? 0 : c === "#" ? 3 : Number(c);
    return `<span class="p${shade}">${c}</span>`;
  });
}

/** HTML for the source; `errorLine` (1-based) gets a wavy underline. */
export function highlight(src, errorLine = 0) {
  let out = "";
  let i = 0;
  const n = src.length;
  while (i < n) {
    const c = src[i];
    const rest = src.slice(i);
    let m;
    if (rest.startsWith("--")) {
      const end = src.indexOf("\n", i);
      const stop = end < 0 ? n : end;
      out += `<span class="cm">${escape(src.slice(i, stop))}</span>`;
      i = stop;
    } else if (rest.startsWith("[[")) {
      const end = src.indexOf("]]", i + 2);
      const stop = end < 0 ? n : end + 2;
      out += escape("[[") + art(escape(src.slice(i + 2, end < 0 ? n : end))) + (end < 0 ? "" : "]]");
      i = stop;
    } else if (c === '"' || c === "'") {
      let j = i + 1;
      while (j < n && src[j] !== c && src[j] !== "\n") j++;
      j = Math.min(j + 1, n);
      out += `<span class="str">${escape(src.slice(i, j))}</span>`;
      i = j;
    } else if ((m = /^(0x[0-9a-fA-F_]+|0b[01_]+|\d[\d_]*)/.exec(rest))) {
      out += `<span class="num">${m[0]}</span>`;
      i += m[0].length;
    } else if ((m = /^[A-Za-z_]\w*/.exec(rest))) {
      const w = m[0];
      const after = src.slice(i + w.length).trimStart()[0];
      const cls = KEYWORDS.has(w) ? "kw" : TYPES.has(w) ? "ty" : BUTTONS.has(w) ? "btn"
        : after === "(" ? "fn" : "";
      out += cls ? `<span class="${cls}">${w}</span>` : w;
      i += w.length;
    } else {
      out += escape(c);
      i++;
    }
  }
  if (errorLine > 0) {
    const lines = out.split("\n");
    if (lines[errorLine - 1] !== undefined) {
      lines[errorLine - 1] = `<span class="errline">${lines[errorLine - 1] || " "}</span>`;
    }
    out = lines.join("\n");
  }
  return out + "\n";
}

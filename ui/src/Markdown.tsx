// Safe markdown rendering for untrusted model output.
//
// `marked` is used only as a lexer; its HTML output is never used. Tokens are
// turned into React elements here, so:
// - raw HTML in the text is shown as literal text, never interpreted;
// - images are never loaded (a remote image is an exfiltration channel); the
//   alt text and URL are shown instead;
// - links are not navigable; they show their full destination and copy it
//   on click, so the user decides what to open, and where;
// - code is inert text.
import { Fragment, memo, ReactNode, useMemo, useState } from "react";
import { Lexer, Token, Tokens } from "marked";

/** Above this size, markdown is not parsed and the text is shown as is. */
const MAX_MARKDOWN_CHARS = 300_000;

const NAMED_ENTITIES: Record<string, string> = {
  amp: "&",
  lt: "<",
  gt: ">",
  quot: '"',
  apos: "'",
  nbsp: " ",
};

/** Decode the few entities markdown text may carry, for display only. */
export function decodeEntities(text: string): string {
  return text.replace(/&(#x[0-9a-fA-F]{1,6}|#[0-9]{1,7}|[a-zA-Z]{2,6});/g, (whole, body: string) => {
    if (body[0] === "#") {
      const code = body[1] === "x" || body[1] === "X" ? parseInt(body.slice(2), 16) : parseInt(body.slice(1), 10);
      const valid = Number.isInteger(code) && code > 0 && code <= 0x10ffff && !(code >= 0xd800 && code <= 0xdfff);
      return valid ? String.fromCodePoint(code) : whole;
    }
    return NAMED_ENTITIES[body] ?? whole;
  });
}

export { copyText } from "./clipboard";
import { copyText } from "./clipboard";

const SHELL_LANGS = new Set([
  "sh", "bash", "zsh", "fish", "shell", "console", "terminal",
  "powershell", "ps", "ps1", "pwsh", "cmd", "bat", "batch",
]);

/** Patterns in model-written commands that deserve a second look. */
const RISKY: [RegExp, string][] = [
  [/\b(curl|wget)\b[^\n|]*\|\s*(sudo\s+)?(ba|z)?sh\b/i, "downloads and immediately runs a script"],
  [/\b(iwr|irm|invoke-webrequest|invoke-restmethod)\b[^\n|]*\|\s*(iex|invoke-expression)\b/i, "downloads and immediately runs a script"],
  [/\b(iex|invoke-expression)\b/i, "runs text as code (Invoke-Expression)"],
  [/\s-e(nc|ncodedcommand)?\s+[A-Za-z0-9+/=]{20,}/i, "runs a hidden, encoded command"],
  [/\bbase64\b[^\n|]*(-d|--decode)[^\n]*\|\s*(ba)?sh\b/i, "decodes and runs hidden code"],
  [/\brm\s+-[a-z]*r[a-z]*\s+(\/|~|\$HOME|\*)(\s|$)/i, "deletes files recursively from a broad location"],
  [/\bremove-item\b[^\n]*-recurse/i, "deletes files recursively"],
  [/\b(diskpart|mkfs(\.\w+)?|format\s+[a-z]:)|\bdd\s+if=/i, "can erase a disk"],
  [/\bset-executionpolicy\b/i, "weakens PowerShell's script protection"],
  [/\b(reg\s+(add|delete)|new-itemproperty|set-itemproperty)\b/i, "changes the Windows registry"],
  [/\b(schtasks|crontab|new-scheduledtask|register-scheduledtask)\b/i, "schedules something to run automatically"],
  [/\bcertutil\b[^\n]*(-decode|-urlcache)/i, "uses certutil to download or decode files"],
  [/\bchmod\s+(-R\s+)?(777|\+s|u\+s)\b/i, "makes files world-writable or setuid"],
  [/\b(sudo|runas)\b/i, "runs with administrator rights"],
  [/\b(add|set)-mppreference\b[^\n]*(exclusion|disable)/i, "weakens Microsoft Defender"],
];

/** Plain-language reasons a command looks risky (empty if none matched). */
export function commandRisks(text: string): string[] {
  const found = new Set<string>();
  for (const [pattern, why] of RISKY) if (pattern.test(text)) found.add(why);
  return [...found];
}

/** The icon copy button (clipboard turns into a check), shared with the rest of the UI. */
import { CopyButton } from "./ui/controls";
export { CopyButton };

function LinkText({ href, children, autolink }: { href: string; children: ReactNode; autolink: boolean }) {
  const [copied, setCopied] = useState(false);
  const url = decodeEntities(href);
  return (
    <span className="md-link-wrap">
      <button
        className="md-link"
        title={`Copy link: ${url}`}
        onClick={async () => {
          if (await copyText(url)) {
            setCopied(true);
            setTimeout(() => setCopied(false), 1200);
          }
        }}
      >
        {children}
      </button>
      {!autolink && <span className="md-url"> ({url})</span>}
      {copied && <span className="md-copied"> copied</span>}
    </span>
  );
}

function inline(tokens: Token[] | undefined, keyPrefix: string): ReactNode[] {
  return (tokens ?? []).map((token, i) => {
    const key = `${keyPrefix}.${i}`;
    switch (token.type) {
      case "text": {
        const t = token as Tokens.Text;
        return t.tokens ? <Fragment key={key}>{inline(t.tokens, key)}</Fragment> : decodeEntities(t.text);
      }
      case "escape":
        return decodeEntities((token as Tokens.Escape).text);
      case "strong":
        return <strong key={key}>{inline((token as Tokens.Strong).tokens, key)}</strong>;
      case "em":
        return <em key={key}>{inline((token as Tokens.Em).tokens, key)}</em>;
      case "del":
        return <del key={key}>{inline((token as Tokens.Del).tokens, key)}</del>;
      case "codespan":
        return <code key={key}>{decodeEntities((token as Tokens.Codespan).text)}</code>;
      case "br":
        return <br key={key} />;
      case "link": {
        const t = token as Tokens.Link & { autolink?: boolean };
        return (
          <LinkText key={key} href={t.href} autolink={Boolean(t.autolink) || t.text === t.href}>
            {inline(t.tokens, key)}
          </LinkText>
        );
      }
      case "image": {
        const t = token as Tokens.Image;
        return (
          <span key={key} className="md-image" title="Images are not loaded">
            [image{t.text ? `: ${decodeEntities(t.text)}` : ""}] {decodeEntities(t.href)}
          </span>
        );
      }
      case "checkbox":
        return (
          <input key={key} type="checkbox" checked={(token as Tokens.Checkbox).checked} disabled readOnly />
        );
      default:
        // Raw HTML and anything unrecognised: literal text.
        return "text" in token && typeof token.text === "string" ? token.text : token.raw;
    }
  });
}

function block(token: Token, key: string): ReactNode {
  switch (token.type) {
    case "space":
    case "def":
      return null;
    case "heading": {
      const t = token as Tokens.Heading;
      const level = Math.min(6, Math.max(1, t.depth));
      return (
        <div key={key} className={`md-h md-h${level}`} role="heading" aria-level={level}>
          {inline(t.tokens, key)}
        </div>
      );
    }
    case "paragraph":
      return <p key={key}>{inline((token as Tokens.Paragraph).tokens, key)}</p>;
    case "text": {
      const t = token as Tokens.Text;
      return <p key={key}>{t.tokens ? inline(t.tokens, key) : decodeEntities(t.text)}</p>;
    }
    case "code": {
      const t = token as Tokens.Code;
      const lang = (t.lang ?? "").split(/\s/)[0]!.slice(0, 32);
      const shell = SHELL_LANGS.has(lang.toLowerCase());
      const risks = commandRisks(t.text);
      return (
        <div key={key} className="md-code">
          <div className="md-code-head">
            <span>{lang || "code"}</span>
            <CopyButton text={t.text} />
          </div>
          {risks.length > 0 ? (
            <div className="md-code-warn" role="note">
              <strong>Check before running.</strong> The model wrote a command that {risks.join("; ")}.
            </div>
          ) : (
            shell && <div className="md-code-note">Written by the model. Read it before running it.</div>
          )}
          <pre>
            <code>{t.text}</code>
          </pre>
        </div>
      );
    }
    case "blockquote":
      return <blockquote key={key}>{blocks((token as Tokens.Blockquote).tokens, key)}</blockquote>;
    case "list": {
      const t = token as Tokens.List;
      const items = t.items.map((item, i) => (
        <li key={`${key}.${i}`} className={item.task ? "md-task" : undefined}>
          {item.loose ? blocks(item.tokens, `${key}.${i}`) : listItemTight(item.tokens, `${key}.${i}`)}
        </li>
      ));
      if (t.ordered) {
        const start = typeof t.start === "number" && t.start >= 0 && t.start < 1e9 ? t.start : 1;
        return (
          <ol key={key} start={start}>
            {items}
          </ol>
        );
      }
      return <ul key={key}>{items}</ul>;
    }
    case "table": {
      const t = token as Tokens.Table;
      const align = (a: string | null) => (a === "center" || a === "right" || a === "left" ? a : undefined);
      return (
        <div key={key} className="md-table">
          <table>
            <thead>
              <tr>
                {t.header.map((cell, i) => (
                  <th key={i} style={{ textAlign: align(t.align[i] ?? null) }}>
                    {inline(cell.tokens, `${key}.h${i}`)}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {t.rows.map((row, r) => (
                <tr key={r}>
                  {row.map((cell, i) => (
                    <td key={i} style={{ textAlign: align(t.align[i] ?? null) }}>
                      {inline(cell.tokens, `${key}.${r}.${i}`)}
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      );
    }
    case "hr":
      return <hr key={key} />;
    case "html":
      // Shown literally, never interpreted.
      return (
        <p key={key} className="md-html">
          {(token as Tokens.HTML).text}
        </p>
      );
    default:
      return (
        <p key={key} className="md-html">
          {token.raw}
        </p>
      );
  }
}

/** Tight list items hold inline-level `text` tokens; avoid paragraph margins. */
function listItemTight(tokens: Token[], key: string): ReactNode[] {
  return tokens.map((token, i) => {
    const k = `${key}.${i}`;
    if (token.type === "text") {
      const t = token as Tokens.Text;
      return <Fragment key={k}>{t.tokens ? inline(t.tokens, k) : decodeEntities(t.text)}</Fragment>;
    }
    if (token.type === "checkbox") return <Fragment key={k}>{inline([token], k)} </Fragment>;
    return block(token, k);
  });
}

function blocks(tokens: Token[], keyPrefix: string): ReactNode[] {
  return tokens.map((t, i) => block(t, `${keyPrefix}.${i}`));
}

/**
 * Memoized: a transcript redraws while a reply streams in, and earlier
 * messages, whose text hasn't changed, skip rebuilding their elements.
 */
export const Markdown = memo(function Markdown({ text }: { text: string }) {
  const tokens = useMemo(() => {
    if (text.length > MAX_MARKDOWN_CHARS) return null;
    try {
      return new Lexer({ gfm: true, breaks: false }).lex(text);
    } catch {
      return null;
    }
  }, [text]);
  if (tokens === null) return <div className="text">{text}</div>;
  return <div className="md">{blocks(tokens, "b")}</div>;
});

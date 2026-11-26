// The chat-template audit, explained: a verdict
// first, what to look for, findings grouped by risk with plain-language
// explanations, the flagged words highlighted, and the full text on demand.
// Every excerpt is untrusted text from the model file: rendered as text
// nodes only, never as markup.
import { ReactNode, useState } from "react";
import { api, ModelRecord } from "./api";

type Risk = "high" | "review" | "info";
type FindingView = ModelRecord["template"]["findings"][number];

function riskOf(f: FindingView): Risk {
  if (f.risk === "high" || f.risk === "review" || f.risk === "info") return f.risk;
  // Records from before risk levels: a warning needs a look.
  return f.severity === "warning" ? "review" : "info";
}

const TITLES: Record<string, string> = {
  url: "Web address",
  email: "Email address",
  encoded: "Encoded data",
  hidden: "Hidden characters",
  construct: "Unusual template code",
  large: "Very large template",
  instruction: "Instruction wording",
  boilerplate: "Known harmless instruction",
  text: "Ordinary text",
  unknown: "Not on the reviewed list",
};

function explanation(f: FindingView, risk: Risk): string {
  const d = f.detail ? `"${f.detail}"` : "";
  switch (f.code) {
    case "url":
      return "A chat template has no reason to contain a link. Templates run on every message, so a link can be used to make the model send your conversation somewhere. Treat this model as possibly poisoned.";
    case "email":
      return "A chat template has no reason to contain an email address. It may be an attempt to make the model send data to someone.";
    case "encoded":
      return "A long block of encoded data (such as base64). It can hide instructions you can't read.";
    case "hidden":
      return "Invisible characters that can hide text from you while the model still reads it.";
    case "construct":
      return `Template code that ordinary chat formatting never needs (${f.detail ?? "unknown"}). It can be used to break out of the template engine or slow it down.`;
    case "large":
      return "The template is unusually large, which makes it easier to hide something in it. Look through the text below.";
    case "instruction":
      return risk === "high"
        ? `Wording about sending data or secrets (${d}). A normal template doesn't talk about passwords, keys, uploads or sending things. Read the highlighted sentence carefully.`
        : `Wording that can change how the model behaves (${d}). Usually a normal rule for tool use or formatting; read the highlighted sentence to check.`;
    case "boilerplate":
      return `A standard instruction from the model family's official template (${d}). It contains a flagged word but is harmless.`;
    case "text":
      return "Text the template adds to conversations, such as a default system prompt, error messages or tool-use instructions.";
    case "unknown":
      return "Gardenwall doesn't have this exact template on its list of reviewed templates, so it asks you to look once. On its own this is not a problem.";
    default:
      return "";
  }
}

/** Plain text with `needle` highlighted (text nodes only). */
function Highlighted({ text, needle }: { text: string; needle: string | null }): ReactNode {
  if (!needle) return text;
  const lower = text.toLowerCase();
  const parts: ReactNode[] = [];
  let at = 0;
  let i = lower.indexOf(needle.toLowerCase());
  while (i >= 0 && parts.length < 40) {
    if (i > at) parts.push(text.slice(at, i));
    parts.push(<mark key={i}>{text.slice(i, i + needle.length)}</mark>);
    at = i + needle.length;
    i = lower.indexOf(needle.toLowerCase(), at);
  }
  parts.push(text.slice(at));
  return <>{parts}</>;
}

function needleFor(f: FindingView): string | null {
  if (f.code === "instruction" || f.code === "boilerplate") return f.detail ?? null;
  if (f.code === "url") {
    const m = /(https?:\/\/|www\.)\S*/i.exec(f.excerpt ?? "");
    return m ? m[0] : null;
  }
  return null;
}

function Finding({ f, risk }: { f: FindingView; risk: Risk }) {
  const [full, setFull] = useState(false);
  const needle = needleFor(f);
  return (
    <li className={`finding finding-${risk}`}>
      <div className="finding-title">
        <span className={`pill ${risk === "high" ? "danger" : risk === "review" ? "warn" : "neutral"}`}>
          {risk === "high" ? "High risk" : risk === "review" ? "Check" : "OK"}
        </span>{" "}
        <strong>{TITLES[f.code ?? ""] ?? f.kind}</strong>
      </div>
      <div className="small finding-why">{explanation(f, risk)}</div>
      {f.excerpt && !full && (
        <div className="excerpt">
          <Highlighted text={f.excerpt} needle={needle} />
        </div>
      )}
      {full && f.context && (
        <div className="excerpt excerpt-full">
          <Highlighted text={f.context} needle={needle} />
        </div>
      )}
      {f.context && f.context !== f.excerpt && (
        <button className="link small" onClick={() => setFull((v) => !v)}>
          {full ? "Show less" : "Show full text"}
        </button>
      )}
    </li>
  );
}

export function TemplateAudit({ model, onChange }: { model: ModelRecord; onChange: () => void }) {
  const t = model.template;
  const [showOrdinary, setShowOrdinary] = useState(false);
  if (!t.present) return <div className="muted small">No embedded chat template.</div>;
  const findings = t.findings.map((f) => ({ f, risk: riskOf(f) }));
  const high = findings.filter((x) => x.risk === "high");
  const review = findings.filter((x) => x.risk === "review");
  const ordinary = findings.filter((x) => x.risk === "info");
  const cleared = !t.requiresAcknowledgement || t.acknowledged;
  const verdict = high.length > 0 ? "high" : review.length > 0 ? "review" : "ok";

  return (
    <details className="audit" open={!cleared}>
      <summary>
        Chat-template audit:{" "}
        {verdict === "high" ? (
          <span className="pill danger">{high.length} high risk</span>
        ) : verdict === "review" ? (
          <span className="pill warn">{review.length} to check</span>
        ) : (
          <span className="pill ok">nothing risky found</span>
        )}{" "}
        {cleared ? <span className="pill ok">reviewed</span> : <span className="pill warn">review required</span>}
      </summary>

      <div className={`audit-verdict audit-verdict-${verdict}`}>
        {verdict === "high" && (
          <p>
            <strong>This template contains something a chat template should never need.</strong> It may have been
            tampered with to make the model leak your conversations. Don't load it unless you trust where it came
            from, and consider downloading the model from its original publisher instead.
          </p>
        )}
        {verdict === "review" && (
          <p>
            <strong>Nothing high-risk was found.</strong> A few sentences use wording that can change how the model
            behaves. This is usually a normal rule for tool use or formatting; read the highlighted sentences below
            to be sure.
          </p>
        )}
        {verdict === "ok" && (
          <p>
            <strong>Nothing risky was found.</strong> The template only contains ordinary formatting and
            instructions.{" "}
            {t.reviewed
              ? "It's exactly a template Gardenwall has reviewed, so you aren't asked to confirm it."
              : "You're asked to confirm once because it isn't on Gardenwall's list of reviewed templates."}
          </p>
        )}
        {t.reviewed && (
          <p className="small muted">
            Reviewed template: {t.reviewed}. Any change to it, even one character, would make it a different
            template that asks again.
          </p>
        )}
      </div>

      <details className="audit-guide">
        <summary>What is the template, and what should I look for?</summary>
        <p className="small">
          The chat template is a small program from the model's publisher that formats every message before the
          model reads it. A tampered template can slip hidden instructions into every conversation. Gardenwall
          always runs the model in a sandbox with no network, so a template can't send anything by itself; the
          risk is that it tells the model to put your data into something you then share or that a tool sends.
        </p>
        <ul className="small">
          <li>
            <strong>Major warning signs:</strong> web addresses (links), email addresses, encoded blocks of text,
            invisible characters, and instructions to send, upload or reveal data, passwords or keys. These have no
            place in a chat template and suggest a poisoned model.
          </li>
          <li>
            <strong>Usually harmless:</strong> rules for calling tools ("reply in the following format"), reasoning
            settings, error messages and default system prompts.
          </li>
        </ul>
      </details>

      {high.length > 0 && (
        <>
          <h4 className="audit-group danger-text">High risk</h4>
          <ul className="findings">
            {high.map(({ f, risk }, i) => (
              <Finding key={`h${i}`} f={f} risk={risk} />
            ))}
          </ul>
        </>
      )}
      {review.length > 0 && (
        <>
          <h4 className="audit-group warn-text">Worth checking</h4>
          <ul className="findings">
            {review.map(({ f, risk }, i) => (
              <Finding key={`r${i}`} f={f} risk={risk} />
            ))}
          </ul>
        </>
      )}
      {ordinary.length > 0 && (
        <>
          <button className="link small" onClick={() => setShowOrdinary((v) => !v)}>
            {showOrdinary ? "Hide" : "Show"} {ordinary.length} ordinary item{ordinary.length === 1 ? "" : "s"}
          </button>
          {showOrdinary && (
            <ul className="findings">
              {ordinary.map(({ f, risk }, i) => (
                <Finding key={`o${i}`} f={f} risk={risk} />
              ))}
            </ul>
          )}
        </>
      )}

      {!cleared && (
        <div className="row audit-ack">
          <button
            className={verdict === "high" ? "danger" : "primary"}
            onClick={async () => {
              if (
                verdict === "high" &&
                !window.confirm(
                  "This template has high-risk findings. Load the model anyway? Only do this if you trust where it came from.",
                )
              )
                return;
              await api.acknowledgeTemplate(model.sha256);
              onChange();
            }}
          >
            {verdict === "high" ? "I understand the risk" : "I've reviewed this template"}
          </button>
          <span className="small muted">Needed once before the model can load.</span>
        </div>
      )}
    </details>
  );
}

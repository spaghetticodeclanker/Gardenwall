// "Choosing a model": the guide in Help and the "What is this?" notes
// beside each term in the model picker (ROADMAP Planned features §6,
// step 2). Kept in one place so the two can't drift apart: a note is the
// first sentences of its guide section.
//
// It explains ideas and names no model as best, so it doesn't date.
// Written 20 November 2026. Static text only.
import { createContext, ReactNode, useContext, useState } from "react";
import { Lightbulb, SlidersHorizontal } from "lucide-react";
import type { HelpRow } from "./helpParts";

export interface GuideSection {
  id: string;
  title: string;
  /** Two or three sentences: the "What is this?" note. */
  short: string;
  /** What you gain and what you give up. */
  gain: string;
  cost: string;
  /** More, for the Help page only. */
  more?: ReactNode;
  /** Which Help topic lists it (default: choosing a model). */
  group?: "generation";
}

export const GUIDE: GuideSection[] = [
  {
    id: "size",
    title: "Model size (parameters)",
    short:
      "A model's size is how many numbers (parameters) it learned, written as 4B for 4 billion or 27B for 27 billion. Larger models usually know more and follow instructions better.",
    gain: "Better answers, more knowledge, more reliable tool use in the agent.",
    cost: "More memory and slower replies. A model must fit in GPU memory to run fast.",
  },
  {
    id: "quant",
    title: "Bits per weight (Q4, Q6, Q8 and IQ files)",
    short:
      "One model is usually offered in several files that store each number with fewer bits (quantization). Q8 keeps 8 bits, Q4 about 4, so a Q4 file is about half the size of a Q8 one; IQ files use a smarter packing for the same size.",
    gain: "Smaller files download faster, need less memory and run faster. Down to about 4 bits the answers stay close to the original.",
    cost: "Below about 4 bits (Q3, Q2, IQ2) quality drops noticeably. A larger model at Q4 is usually better than a smaller one at Q8.",
    more: (
      <p>
        The letters after the number are variants: K files (Q4_K_M) keep the most sensitive parts at higher precision; M
        and S mean medium and small. QAT in a file name means the model was trained to be stored at that precision, so
        it loses less than usual.
      </p>
    ),
  },
  {
    id: "context",
    title: "Context length",
    short:
      "The context is how much text the model can keep in mind at once: your messages, its replies, attached documents and the agent's tool results, measured in tokens (about three quarters of a word each).",
    gain: "A long context lets the model work with long documents and long agent sessions.",
    cost: "The context lives in GPU memory next to the model. A model that only just fits leaves room for short conversations only.",
  },
  {
    id: "moe",
    title: "Dense and Mixture-of-Experts models",
    short:
      "A dense model uses all its numbers for every word. A Mixture-of-Experts model (MoE, often written like 26B-A4B) has many small expert parts and uses only a few of them per word.",
    gain: "An MoE model knows as much as a large model but replies about as fast as a small one, and it still runs acceptably when part of it is in system RAM.",
    cost: "It needs the memory of its full size, and at equal size a dense model is often a little better at careful reasoning.",
  },
  {
    id: "vision",
    title: "Vision models and their projector file",
    short:
      "A vision model can look at pictures and screenshots. It needs a second, smaller file called an image projector (mmproj), which turns an image into something the model reads.",
    gain: "Ask about screenshots, photos, charts and scanned pages.",
    cost: "The projector takes more memory, and each image uses part of the context.",
  },
  {
    id: "mtp",
    title: "Built-in faster generation (MTP)",
    short:
      "Some models include an extra layer that guesses the next few words ahead (multi-token prediction, MTP). Gardenwall turns it on automatically when there is room.",
    gain: "Replies are written faster with the same answers: about 1.7 to 2.4 times as fast in Gardenwall's tests on a 27B model.",
    cost: "About 1 GiB more GPU memory, and reading long prompts is slightly slower.",
  },
  {
    id: "thinking",
    title: "Thinking",
    short:
      "Thinking models write out their reasoning before the answer. You can see it in a fold above the reply, and some models let you choose how much they think.",
    gain: "Better answers to hard questions, maths and code.",
    cost: "Each answer takes longer and uses more of the context.",
  },
  {
    id: "licence",
    title: "Licences",
    short:
      "Every model comes with a licence from its publisher that says how you may use it. Many allow almost any use (Apache 2.0, MIT); some forbid commercial use or add conditions.",
    gain: "Open licences let you use a model for work and share what you make with it.",
    cost: "Read the licence on the model's page before using a model for work. Gardenwall shows the licence the publisher declared but can't judge it for you.",
  },
  {
    id: "gated",
    title: "Gated models",
    short:
      "A gated model is one whose publisher only lets signed-in Hugging Face accounts download it, after accepting its terms. Gardenwall downloads without an account, so it can't download these.",
    gain: "Nothing for you: gating is the publisher's choice.",
    cost: "To use one, download it in your browser after accepting the terms, then import the file on the Models page. It is checked the same way.",
  },
  {
    id: "abliterated",
    title: "Stock, abliterated and uncensored models",
    short:
      "A stock model is published as its maker released it, refusals included. An abliterated model has its refusals removed by editing the weights directly, without retraining; an uncensored fine-tune is retrained on other data instead.",
    gain: "Abliterated and uncensored models refuse less, which some people want for fiction, research or security work.",
    cost: "What either change costs in answer quality is not well measured; changing a model this way can make it less accurate. These models come from third parties, not the original maker.",
    more: (
      <p>
        Gardenwall's protection doesn't depend on which model you choose or on a model refusing anything: every model is
        treated as untrusted, runs in a sandbox, and can only do what you allow. The starter list offers stock models
        only; the others are reachable through the ranking, search and research.
      </p>
    ),
  },
  {
    id: "temperature",
    group: "generation",
    title: "Temperature",
    short:
      "How adventurous the model is when it picks each next word. At 0 it always takes the likeliest word; higher values let it pick less likely ones more often.",
    gain: "Higher (around 0.8–1.0) gives more varied, creative writing. Lower (0.2–0.6) gives steadier, more repeatable answers, which suits code, facts and the agent.",
    cost: "Too high and answers wander, invent things or lose the thread; too low and they can get repetitive. Many models publish a recommended value, which leaving the field empty uses.",
  },
  {
    id: "top-p",
    group: "generation",
    title: "Top-p (nucleus sampling)",
    short:
      "Limits each choice to the likeliest words that together make up this share of the probability. At 0.9 the model picks from the words covering 90 % of the chance and ignores the long tail.",
    gain: "Cuts off rare, odd words while keeping several sensible ones, so answers stay coherent without being flat.",
    cost: "Lower values (0.5) make answers safer but blander; 1 turns it off. It overlaps with Min-p, so changing both at once is rarely needed.",
  },
  {
    id: "top-k",
    group: "generation",
    title: "Top-k",
    short: "Limits each choice to the k likeliest words, whatever their chances. At 40 the model only ever considers its top 40 candidates.",
    gain: "A simple hard cap that stops very unlikely words from ever being picked.",
    cost: "It ignores how confident the model is: 40 can be too many when one word is obvious and too few when many fit. 0 turns it off. Top-p or Min-p usually adapt better.",
  },
  {
    id: "min-p",
    group: "generation",
    title: "Min-p",
    short:
      "Drops any word whose chance is below this fraction of the likeliest word's chance. At 0.05, a word must be at least 5 % as likely as the top choice to be considered.",
    gain: "Adapts to the model's confidence: strict when one answer is clear, looser when many fit. It works well with a higher temperature for creative writing.",
    cost: "Too high (above about 0.2) and replies become predictable; 0 turns it off.",
  },
  {
    id: "max-tokens",
    group: "generation",
    title: "Max response tokens",
    short: "The longest reply the model may write, in tokens (about three quarters of a word each). Empty means no limit other than the context.",
    gain: "A limit stops runaway replies and keeps answers short when you want them short.",
    cost: "Set too low, replies stop mid-sentence, and thinking models can use the whole budget thinking before they answer.",
  },
];

const BY_ID = new Map(GUIDE.map((g) => [g.id, g]));

function GuideSections({ sections }: { sections: GuideSection[] }) {
  return (
    <>
      {sections.map((g) => (
        <div key={g.id} id={`help-guide-${g.id}`} className="guide-section">
          <h4>{g.title}</h4>
          <p>{g.short}</p>
          <ul>
            <li>
              <strong>Gain:</strong> {g.gain}
            </li>
            <li>
              <strong>Cost:</strong> {g.cost}
            </li>
          </ul>
          {g.more}
        </div>
      ))}
    </>
  );
}

/** The opening paragraph of Help's "Generation settings" topic. */
export function GenerationIntro() {
  return (
    <p>
      These settings in <em>Settings → Generation</em> shape how the model picks each word of a reply. Leave them empty to
      use each model's own defaults, which are usually right; change one at a time and try a few replies.
    </p>
  );
}

/** The Help page's "Generation settings" topic. */
export function GenerationGuideBody() {
  return (
    <>
      <GenerationIntro />
      <GuideSections sections={GUIDE.filter((g) => g.group === "generation")} />
    </>
  );
}

/**
 * The guide's entries as folding rows for the redesigned Help page
 * (docs/internal/design/HELP-REDESIGN.md): the title, the first sentence as the
 * one-line summary, and the full entry inside, unchanged. The anchor keeps
 * "More in the guide" links working.
 */
export function guideRows(kind: "choosing" | "generation"): HelpRow[] {
  return GUIDE.filter((g) => (kind === "generation" ? g.group === "generation" : !g.group)).map((g) => ({
    anchor: `help-guide-${g.id}`,
    icon: kind === "generation" ? SlidersHorizontal : Lightbulb,
    title: g.title,
    summary: firstSentence(g.short),
    body: (
      <>
        <p>{g.short}</p>
        <ul>
          <li>
            <strong>Gain:</strong> {g.gain}
          </li>
          <li>
            <strong>Cost:</strong> {g.cost}
          </li>
        </ul>
        {g.more}
      </>
    ),
  }));
}

function firstSentence(text: string): string {
  const m = /^(.+?[.!?])(\s|$)/.exec(text);
  return m?.[1] ?? text;
}

/** Which Help topic holds a guide entry. */
export function guideTopicOf(id: string): "choosing" | "generation" {
  return BY_ID.get(id)?.group === "generation" ? "generation" : "choosing";
}

/** Opens Help at a guide section (set by the app). */
export const GuideNav = createContext<(id: string) => void>(() => {});

/** The opening paragraph of Help's "Choosing a model" topic. */
export function GuideIntro() {
  return (
    <p>
      Gardenwall can suggest a model for this computer on the <em>Models</em> page. Each idea below has something you
      gain and something you give up. Gardenwall says what fits and what a choice costs; it never says a model is safe
      or good.
    </p>
  );
}

/** The Help page's "Choosing a model" topic. */
export function GuideBody() {
  return (
    <>
      <GuideIntro />
      <GuideSections sections={GUIDE.filter((g) => !g.group)} />
    </>
  );
}

/** A term with a "What is this?" note that opens in place. */
export function Term({ id, children }: { id: string; children: ReactNode }) {
  const [open, setOpen] = useState(false);
  const openGuide = useContext(GuideNav);
  const g = BY_ID.get(id);
  if (!g) return <>{children}</>;
  return (
    <span className="term">
      {children}{" "}
      <button
        className="link small what-is"
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
        title={`What is this? (${g.title})`}
      >
        {open ? "Close" : "What is this?"}
      </button>
      {open && (
        <span className="term-note small" role="note">
          {g.short}{" "}
          <button className="link small" onClick={() => openGuide(g.id)}>
            More in the guide
          </button>
        </span>
      )}
    </span>
  );
}

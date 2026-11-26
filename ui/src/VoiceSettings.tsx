// Settings → Voice (release plan features 7 and 8): voice input and read
// aloud. Menus and switches save at once.
import { useEffect, useState } from "react";
import { Download, Volume2 } from "lucide-react";
import { api, AppView, DictationStatus, errorText, SetupProgress, VoiceInfo, VoiceSettings } from "./api";
import { readAloud } from "./speech";
import { Button, SettingGroup, SettingRow, Toggle, useToast } from "./ui";

const RATES: [number, string][] = [
  [0.75, "Slower"],
  [1, "Normal"],
  [1.25, "A little faster"],
  [1.5, "Faster"],
  [2, "Much faster"],
];

/** Languages offered for voice input (whisper.cpp codes). */
const LANGUAGES: [string, string][] = [
  ["en", "English"], ["ar", "Arabic"], ["zh", "Chinese"], ["cs", "Czech"], ["da", "Danish"], ["nl", "Dutch"],
  ["fi", "Finnish"], ["fr", "French"], ["de", "German"], ["el", "Greek"], ["he", "Hebrew"], ["hi", "Hindi"],
  ["id", "Indonesian"], ["it", "Italian"], ["ja", "Japanese"], ["ko", "Korean"], ["no", "Norwegian"],
  ["pl", "Polish"], ["pt", "Portuguese"], ["ru", "Russian"], ["es", "Spanish"], ["sv", "Swedish"],
  ["th", "Thai"], ["tr", "Turkish"], ["uk", "Ukrainian"], ["vi", "Vietnamese"],
];

function gb(bytes: number): string {
  return bytes >= 1e9 ? `${(bytes / 1e9).toFixed(1)} GB` : `${Math.max(1, Math.round(bytes / 1e6))} MB`;
}

const ENGINES: [VoiceSettings["inputEngine"], string][] = [
  ["auto", "Automatic"],
  ["gpu", "Graphics card"],
  ["cpu", "Processor"],
  ["off", "Off"],
];

function VoiceInput({ saved, apply }: { saved: VoiceSettings; apply: (p: Partial<VoiceSettings>, label: string) => Promise<void> }) {
  const [status, setStatus] = useState<DictationStatus | null>(null);
  const [progress, setProgress] = useState<SetupProgress | null>(null);
  const [error, setError] = useState<string | null>(null);
  const load = () => api.dictationStatus().then(setStatus).catch((e) => setError(errorText(e)));
  useEffect(() => {
    void load();
  }, []);
  const setup = async () => {
    setError(null);
    setProgress({ step: "Starting…", done: 0, total: 1 });
    try {
      await api.dictationSetup(setProgress);
    } catch (e) {
      setError(errorText(e));
    } finally {
      setProgress(null);
      await load();
    }
  };
  if (!status) return null;
  const engine = saved.inputEngine;
  const ready = status.cpuReady || status.gpuReady;
  const complete = status.downloadBytes === 0;
  const readyText =
    engine === "gpu" ? "Ready (graphics card)" : engine === "cpu" ? "Ready (processor)" : status.gpuReady ? "Ready (graphics card and processor)" : "Ready (processor)";
  return (
    <SettingGroup title="Voice input">
      <SettingRow
        label="Recogniser"
        help="Automatic uses the large model on your graphics card when it fits beside the loaded chat model, otherwise the small one on the processor. Graphics card always uses the large one (it can slow the chat model if the card is nearly full); Processor always uses the small one and leaves the card alone. Off hides the microphone."
      >
        <select
          id="voice-input-engine"
          value={engine}
          onChange={async (e) => {
            const next = e.target.value as VoiceSettings["inputEngine"];
            await apply({ inputEngine: next }, next === "off" ? "Voice input is off" : "Recogniser saved");
            await load();
          }}
        >
          {ENGINES.filter(([id]) => id !== "gpu" || status.gpuPresent || engine === "gpu").map(([id, label]) => (
            <option key={id} value={id}>
              {label}
            </option>
          ))}
        </select>
      </SettingRow>
      {engine !== "off" && (
      <SettingRow
        label="Speech recogniser"
        help="Whisper, turned into text on this computer in a sandbox with no network. The microphone is opened by Gardenwall itself, only while you're recording, and the audio is never saved. Large model on your graphics card when it fits beside the loaded model, otherwise a small one on the processor."
      >
        {!status.supported ? (
          <span className="small muted">Available on Windows only.</span>
        ) : progress ? (
          <span className="small">
            {progress.step}… {progress.total > 1 ? `${Math.floor((progress.done / progress.total) * 100)}%` : ""}
          </span>
        ) : complete ? (
          <span className="small">{readyText}</span>
        ) : (
          <Button icon={Download} onClick={() => void setup()}>
            {ready ? "Finish setting up" : "Set up voice input"} ({gb(status.downloadBytes)})
          </Button>
        )}
      </SettingRow>
      )}
      {engine !== "off" && (
      <>
      <SettingRow label="Language" help="The language you speak. Choosing it makes recognition more accurate than guessing.">
        <select id="voice-input-language" value={saved.inputLanguage} onChange={(e) => void apply({ inputLanguage: e.target.value }, "Language saved")}>
          {LANGUAGES.map(([code, name]) => (
            <option key={code} value={code}>
              {name}
            </option>
          ))}
        </select>
      </SettingRow>
      <SettingRow
        label="Tidy what I say"
        help="The loaded chat model fixes punctuation and removes filler words such as “um”. Your exact words are one click away in the notice that follows. Skipped when no model is loaded."
      >
        <Toggle
          checked={saved.inputCleanup}
          label="Tidy what I say"
          onChange={(on) => void apply({ inputCleanup: on }, on ? "Transcripts will be tidied" : "Transcripts will be left as said")}
        />
      </SettingRow>
      <SettingRow label="How to use it">
        <span className="small muted">Click the microphone in any message box, or hold Ctrl+Space while you speak. The words appear as you speak.</span>
      </SettingRow>
      </>
      )}
      {error && <div className="banner error">{error}</div>}
    </SettingGroup>
  );
}

const SAMPLE =
  "This is how replies sound when Gardenwall reads them aloud. Code blocks and tool output are announced, not read.";

export function VoiceSettingsSection({ state, refresh }: { state: AppView; refresh: () => Promise<void> }) {
  const saved = state.voiceSettings;
  const [voices, setVoices] = useState<VoiceInfo[] | null>(null);
  const [voicesError, setVoicesError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const toast = useToast();

  useEffect(() => {
    api
      .speechVoices()
      .then(setVoices)
      .catch((e) => setVoicesError(errorText(e)));
  }, []);

  const apply = async (patch: Partial<VoiceSettings>, label: string) => {
    setError(null);
    try {
      await api.setVoiceSettings({ ...saved, ...patch });
      await refresh();
      toast({ text: label, tone: "ok" });
    } catch (e) {
      setError(errorText(e));
    }
  };

  const rateValue = RATES.some(([r]) => r === saved.readRate) ? saved.readRate : 1;

  return (
    <section>
      <h2>Voice</h2>
      <VoiceInput saved={saved} apply={apply} />
      <SettingGroup title="Read aloud">
        <SettingRow
          label="Voice"
          help="Windows' own voices, spoken on this computer inside a sandbox with no network. Add more in Windows Settings → Time & language → Speech."
        >
          {voicesError ? (
            <span className="small muted">{voicesError}</span>
          ) : (
            <select
              id="voice-read-voice"
              value={saved.readVoice ?? ""}
              disabled={!voices}
              onChange={(e) => void apply({ readVoice: e.target.value || null }, "Voice saved")}
            >
              <option value="">Windows' default voice</option>
              {(voices ?? []).map((v) => (
                <option key={v.id} value={v.id}>
                  {v.name}
                  {v.language ? ` (${v.language})` : ""}
                </option>
              ))}
            </select>
          )}
        </SettingRow>
        <SettingRow label="Speed">
          <select
            id="voice-read-rate"
            value={String(rateValue)}
            onChange={(e) => void apply({ readRate: Number(e.target.value) }, "Speed saved")}
          >
            {RATES.map(([r, label]) => (
              <option key={r} value={String(r)}>
                {label}
              </option>
            ))}
          </select>
        </SettingRow>
        <SettingRow
          label="Read new replies automatically"
          help="Each chat and agent reply is read when it finishes. Off by default; the Read aloud button on any reply works either way."
        >
          <Toggle
            checked={saved.readAuto}
            label="Read new replies automatically"
            onChange={(on) => void apply({ readAuto: on }, on ? "New replies will be read aloud" : "New replies won't be read aloud")}
          />
        </SettingRow>
        <SettingRow label="Try it">
          <Button icon={Volume2} onClick={() => void readAloud(SAMPLE)}>
            Play a sample
          </Button>
        </SettingRow>
      </SettingGroup>
      {error && <div className="banner error">{error}</div>}
    </section>
  );
}

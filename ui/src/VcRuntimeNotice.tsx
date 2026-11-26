// Microsoft's Visual C++ runtime is missing: the engines can't start without
// it. Gardenwall asks the user to install
// Microsoft's package (it doesn't ship or install it). Addresses are shown
// with Copy buttons; the app never opens web addresses itself.
import { useState } from "react";
import type { AppView } from "./api";
import { copyText } from "./clipboard";
import { Button } from "./ui";

const DOWNLOAD_FILE = "https://aka.ms/vs/17/release/vc_redist.x64.exe";
const DOWNLOAD_PAGE = "https://learn.microsoft.com/cpp/windows/latest-supported-vc-redist";

export function VcRuntimeNotice({ state, refresh }: { state: AppView; refresh: () => Promise<void> }) {
  const [copied, setCopied] = useState<string | null>(null);
  const [checking, setChecking] = useState(false);
  if (state.vcRuntimeMissing.length === 0) return null;
  const copy = async (text: string) => {
    if (await copyText(text)) setCopied(text);
  };
  return (
    <div className="banner warn gw-vcruntime" role="alert">
      <div>
        <strong>One more thing from Microsoft is needed.</strong> Gardenwall's engine needs the Microsoft Visual C++
        Redistributable, which this PC doesn't have yet ({state.vcRuntimeMissing.join(", ")} missing). Without it, models and
        voice input can't start.
      </div>
      <ol className="small">
        <li>
          Download Microsoft's installer:{" "}
          <span className="mono">{DOWNLOAD_FILE}</span>{" "}
          <button type="button" className="link" onClick={() => void copy(DOWNLOAD_FILE)}>
            {copied === DOWNLOAD_FILE ? "Copied" : "Copy"}
          </button>
          <div className="muted">
            It's listed on Microsoft's page <span className="mono">{DOWNLOAD_PAGE}</span>{" "}
            <button type="button" className="link" onClick={() => void copy(DOWNLOAD_PAGE)}>
              {copied === DOWNLOAD_PAGE ? "Copied" : "Copy"}
            </button>
          </div>
        </li>
        <li>Run it and choose Install (Windows asks for permission).</li>
        <li>Come back here and choose Check again. You don't need to restart Gardenwall.</li>
      </ol>
      <div>
        <Button
          size="sm"
          disabled={checking}
          onClick={async () => {
            setChecking(true);
            try {
              await refresh();
            } finally {
              setChecking(false);
            }
          }}
        >
          {checking ? "Checking…" : "Check again"}
        </Button>
      </div>
    </div>
  );
}

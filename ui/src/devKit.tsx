// Development-only: the building-blocks page for owner review (mock.html?kit=1).
// Loaded only by devMock.ts; never part of the desktop app.
import { StrictMode, useState } from "react";
import { createRoot } from "react-dom/client";
import { Cpu, Download, FileText, FolderOpen, HardDrive, HelpCircle, Plus, RefreshCw, Settings, Trash2 } from "lucide-react";
import "./fonts";
import "./styles.css";
import {
  Button,
  CopyButton,
  DataTable,
  Disclosure,
  EmptyState,
  ErrorCard,
  IconButton,
  Inspector,
  Menu,
  Pill,
  SegmentedBar,
  SettingGroup,
  SettingRow,
  ToastHost,
  Toggle,
  useToast,
} from "./ui";

interface ModelRow {
  id: string;
  name: string;
  quant: string;
  size: number;
  fit: "gpu" | "ram" | "no";
}

const MODELS: ModelRow[] = [
  { id: "a", name: "Huihui-Qwen3.8-27B-abliterated", quant: "Q4_K_M", size: 16.6, fit: "gpu" },
  { id: "b", name: "Qwen3-0.6B", quant: "Q8_0", size: 0.6, fit: "gpu" },
  { id: "c", name: "A-very-long-model-name-that-would-have-squished-the-old-table-70B-Instruct", quant: "Q4_K_S", size: 40.2, fit: "ram" },
];

const FIT = { gpu: <Pill tone="ok">Fits GPU</Pill>, ram: <Pill tone="warn">Part in RAM</Pill>, no: <Pill tone="danger">Too large</Pill> };

function Sprout() {
  return (
    <svg width="120" height="72" viewBox="0 0 120 72">
      <g fill="none" stroke="var(--accent)" strokeOpacity=".35" strokeLinecap="round">
        <path d="M60 72C60 56 58 46 62 35S70 18 66 8" />
        <path d="M61 51C72 47 80 41 86 31" />
        <path d="M62 39C50 35 44 27 38 19" />
      </g>
      <g fill="var(--accent)" fillOpacity=".5">
        <circle cx="66" cy="8" r="2.4" />
        <circle cx="86" cy="31" r="2" />
        <circle cx="38" cy="19" r="2" />
      </g>
    </svg>
  );
}

function Kit() {
  const toast = useToast();
  const [cards, setCards] = useState(true);
  const [web, setWeb] = useState(false);
  const [sel, setSel] = useState<string | null>("a");
  const [tab, setTab] = useState("changes");
  return (
    <div style={{ display: "flex", height: "100vh" }}>
      <main className="page" style={{ flex: 1, padding: "28px 32px 64px" }}>
        <div style={{ maxWidth: 880 }}>
          <h2 style={{ fontSize: "var(--fs-xl)", letterSpacing: "-0.01em", marginBottom: 4 }}>Building blocks</h2>
          <p className="muted" style={{ marginTop: 0 }}>
            Moss night tokens, Inter and JetBrains Mono, Lucide icons. Phase 0 of the redesign, for review.
          </p>

          <h3>Buttons</h3>
          <div className="row" style={{ gap: 8 }}>
            <Button variant="primary" icon={Download}>Load model</Button>
            <Button>Choose another</Button>
            <Button variant="ghost" icon={RefreshCw}>Check again</Button>
            <Button variant="danger" icon={Trash2}>Delete</Button>
            <Button variant="primary" disabled>Disabled</Button>
            <Button size="sm">Small</Button>
            <IconButton icon={Settings} label="Settings" />
            <CopyButton text="cargo test --workspace" />
            <span className="muted small">← copy turns into a check</span>
          </div>

          <h3>Menu</h3>
          <Menu
            label="Gardenwall menu"
            trigger={(p) => <IconButton icon={Settings} label="Open the menu" {...p} />}
            align="start"
            items={[
              { label: "Models", icon: HardDrive, onSelect: () => toast({ text: "Models" }) },
              { label: "Runtime", icon: Cpu, onSelect: () => toast({ text: "Runtime" }) },
              { label: "Documents", icon: FileText, onSelect: () => toast({ text: "Documents" }) },
              "separator",
              { label: "Settings", icon: Settings, onSelect: () => toast({ text: "Settings" }) },
              { label: "Help", icon: HelpCircle, onSelect: () => toast({ text: "Help" }) },
            ]}
          />

          <h3>Settings rows</h3>
          <SettingGroup title="Agent">
            <SettingRow label="Shell" help="Commands run in this shell inside the sandbox.">
              <select defaultValue="pwsh">
                <option value="pwsh">PowerShell 7</option>
                <option value="ps5">Windows PowerShell 5.1</option>
                <option value="cmd">Command Prompt</option>
              </select>
            </SettingRow>
            <SettingRow label='"Run this yourself" cards' help="The agent can ask you to run a command that needs the network.">
              <Toggle
                label='"Run this yourself" cards'
                checked={cards}
                onChange={(v) => {
                  setCards(v);
                  toast({ text: v ? "Cards turned on" : "Cards turned off", tone: "ok", action: { label: "Undo", run: () => setCards(!v) } });
                }}
              />
            </SettingRow>
            <SettingRow label="Web research in sessions" help="Off: the agent can't search or read the web.">
              <Toggle label="Web research in sessions" checked={web} onChange={setWeb} />
            </SettingRow>
          </SettingGroup>
          <Disclosure summary="Advanced">
            <SettingGroup>
              <SettingRow label="Long tasks: pause after" help="The turn pauses with a Continue / Stop card.">
                <select defaultValue="100">
                  <option>50</option>
                  <option value="100">100 steps</option>
                  <option>200</option>
                </select>
              </SettingRow>
            </SettingGroup>
          </Disclosure>

          <h3>Table</h3>
          <DataTable<ModelRow>
            rows={MODELS}
            rowKey={(m) => m.id}
            selected={sel}
            onSelect={(m) => setSel(m.id)}
            columns={[
              { key: "name", header: "Model", minWidth: "220px", render: (m) => <span>{m.name}</span> },
              { key: "quant", header: "Quantization", width: "120px", render: (m) => <span className="mono">{m.quant}</span> },
              { key: "size", header: "Size", width: "90px", align: "right", render: (m) => `${m.size.toFixed(1)} GB` },
              { key: "fit", header: "Fit", width: "120px", render: (m) => FIT[m.fit] },
              {
                key: "act",
                header: <span className="gw-sr">Actions</span>,
                width: "96px",
                align: "right",
                render: (m) => <Button size="sm" variant={m.id === "a" ? "primary" : "secondary"}>Load</Button>,
              },
            ]}
          />

          <h3>Context bar</h3>
          <SegmentedBar
            label="Context"
            total={32768}
            marker={{ at: 0.8, label: "Summarises at 80%" }}
            legend
            segments={[
              { label: "System", value: 2600, color: "#2f6b4c" },
              { label: "Tools", value: 3900, color: "#3e8c63" },
              { label: "Conversation", value: 6900, color: "--accent" },
            ]}
          />

          <h3>GPU and RAM</h3>
          <div style={{ display: "grid", gap: 20 }}>
            <div>
              <div style={{ display: "flex", justifyContent: "space-between", marginBottom: 8 }}>
                <span>RTX 5090</span>
                <span className="muted small" style={{ fontVariantNumeric: "tabular-nums" }}>24.3 of 32 GB</span>
              </div>
              <SegmentedBar
                label="GPU memory"
                total={32}
                height={10}
                legend
                format={(n) => `${n.toFixed(1)} GB`}
                segments={[
                  { label: "Model", value: 16.6, color: "--accent" },
                  { label: "Context", value: 5.8, color: "#3e8c63" },
                  { label: "Other apps", value: 1.9, color: "#6b7a72" },
                  { label: "Safety margin", value: 3, color: "", hatched: true },
                ]}
              />
            </div>
            <div>
              <div style={{ display: "flex", justifyContent: "space-between", marginBottom: 8 }}>
                <span>RAM</span>
                <span className="muted small" style={{ fontVariantNumeric: "tabular-nums" }}>22.1 of 64 GB</span>
              </div>
              <SegmentedBar
                label="RAM"
                total={64}
                height={10}
                legend
                format={(n) => `${n.toFixed(1)} GB`}
                segments={[
                  { label: "Gardenwall", value: 2.4, color: "--accent" },
                  { label: "Other apps", value: 19.7, color: "#6b7a72" },
                ]}
              />
            </div>
          </div>

          <h3>Pills</h3>
          <div className="row">
            <Pill tone="ok">Fits GPU</Pill>
            <Pill tone="accent">Template reviewed</Pill>
            <Pill tone="warn">Part in RAM</Pill>
            <Pill tone="danger">Too large</Pill>
            <Pill>OCR · 12 pages</Pill>
          </div>

          <h3>Empty state</h3>
          <div className="gw-group-body">
            <EmptyState
              art={<Sprout />}
              title="Start your first session"
              body="Pick a folder and tell the agent what to do. It can only touch what you allow."
              action={<Button variant="primary" icon={FolderOpen}>New session</Button>}
            />
          </div>

          <h3>Error</h3>
          <ErrorCard
            title="The model stopped responding"
            cause="llama-server exited while loading. This usually means it ran out of GPU memory."
            fix={<Button variant="primary">Load with less context</Button>}
            details={"llama_model_load: error loading model: unable to allocate CUDA0 buffer\nexit code 1"}
          />

          <h3>Toasts</h3>
          <div className="row">
            <Button onClick={() => toast({ text: "Settings saved", tone: "ok", action: { label: "Undo", run: () => {} } })}>Saved</Button>
            <Button onClick={() => toast({ text: "Fix the greeting paused at 100 steps", tone: "warn", sticky: true, action: { label: "Open", run: () => {} } })}>
              Paused (stays)
            </Button>
            <Button onClick={() => toast({ text: "Download failed: the connection closed", tone: "danger" })}>Failed</Button>
          </div>

          <h3>Type</h3>
          <div style={{ display: "grid", gap: 6 }}>
            <div style={{ fontSize: "var(--fs-xl)", fontWeight: 600, letterSpacing: "-0.015em" }}>Heading 23 · Load a model to start</div>
            <div style={{ fontSize: "var(--fs-lg)", fontWeight: 600, letterSpacing: "-0.01em" }}>Heading 19 · Fix the greeting</div>
            <div style={{ fontSize: "var(--fs-md)", fontWeight: 600 }}>Heading 16 · Recent sessions</div>
            <div>Body 14 · Everything runs on this computer. Nothing leaves it unless you allow it.</div>
            <div className="muted small">Caption 12 · Saved from Agent · applies to new chats</div>
            <div className="mono" style={{ fontFamily: "var(--font-mono)", fontSize: 12.5 }}>W:\greeter\src\greet.rs · 13,442 / 32,768</div>
          </div>
        </div>
      </main>
      <Inspector
        title="Session details"
        active={tab}
        onActive={setTab}
        onClose={() => toast({ text: "The panel would close" })}
        tabs={[
          {
            id: "changes",
            label: "Changes",
            badge: 3,
            content: (
              <div style={{ display: "grid", gap: 10 }}>
                {["src/greet.rs +4 −1", "tests/greet.rs +12", "README.md +1 −1"].map((f) => (
                  <div key={f} style={{ display: "flex", alignItems: "center", gap: 8 }}>
                    <span className="mono" style={{ fontFamily: "var(--font-mono)", fontSize: 12, flex: 1 }}>{f}</span>
                    <Button size="sm" variant="ghost">Put back</Button>
                  </div>
                ))}
              </div>
            ),
          },
          { id: "context", label: "Context", badge: "41%", content: <p className="muted">The full context breakdown goes here.</p> },
          { id: "tasks", label: "Tasks", badge: "2/5", content: <p className="muted">The task list goes here.</p> },
          { id: "skills", label: "Skills", content: <Button icon={Plus}>Add a skill</Button> },
        ]}
      />
    </div>
  );
}

const root = document.getElementById("root");
if (root) {
  createRoot(root).render(
    <StrictMode>
      <ToastHost>
        <Kit />
      </ToastHost>
    </StrictMode>,
  );
}

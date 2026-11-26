// Settings → Binary analysis: the built-in
// analyzer needs no setup; Ghidra is optional, for decompiling to C.
import { useEffect, useState } from "react";
import { Button, Fold, Pill, SettingGroup, SettingRow } from "./ui";
import { api, BinarySettingsView, errorText } from "./api";
import { ProsCons } from "./ModelSettings";
import { shownPath } from "./paths";

export function BinarySettings() {
  const [view, setView] = useState<BinarySettingsView | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    api.binarySettings().then(setView).catch((e) => setError(errorText(e)));
  }, []);
  const set = async (which: "ghidra" | "java" | "detectJava" | "clear") => {
    setError(null);
    try {
      setView(await api.setBinaryFolder(which));
    } catch (e) {
      setError(errorText(e));
    }
  };
  return (
    <section>
      <h2>Binary analysis</h2>
      <Fold summary="The agent can examine programs and libraries without running them.">
        <p>
          It reads EXE, DLL, SYS, ELF and Mach-O files: headers, security flags, imports and exports, strings, functions,
          hex and x86/x64 disassembly. Attaching a program in chat adds the same report to your message. Files are read
          by a separate helper in its own sandbox with no network and no other file access, and are never executed.
        </p>
        <p>
          The built-in analyzer works with no setup. For readable C code, install Ghidra (free, from the NSA's GitHub
          releases) and a Java JDK 21 or newer, then choose their folders below. The agent then gets a decompile tool.
          Ghidra runs in its own sandbox: no network, its own folder and the JDK read-only, and a private project folder.
        </p>
        <ProsCons
          pros={[
            "Decompiled C is much easier for a model to reason about than assembly.",
            "Each analyzed file's Ghidra project is kept, so later functions decompile in seconds.",
          ]}
          cons={[
            "The first decompile of a file can take minutes and several GB of memory.",
            "Ghidra and the JDK are large downloads that Gardenwall doesn't install or update for you.",
          ]}
        />
      </Fold>
      {error && <div className="banner error">{error}</div>}
      {view && (
        <>
          <SettingGroup>
            <SettingRow label="Built-in analyzer" help="Headers, imports, strings, functions and disassembly. No setup needed.">
              <Pill tone="ok">Ready</Pill>
            </SettingRow>
            <SettingRow
              label="Ghidra"
              help={
                view.ghidraDir ? (
                  <>
                    <span className="mono gw-settings-path" title={shownPath(view.ghidraDir)}>
                      {shownPath(view.ghidraDir)}
                    </span>
                    {view.ghidraVersion && <> · version {view.ghidraVersion}</>}
                  </>
                ) : (
                  "Not set. Optional, for decompiling to C."
                )
              }
            >
              <Button size="sm" onClick={() => void set("ghidra")}>
                Choose folder…
              </Button>
            </SettingRow>
            <SettingRow
              label="Java JDK"
              help={
                view.javaHome ? (
                  <span className="mono gw-settings-path" title={shownPath(view.javaHome)}>
                    {shownPath(view.javaHome)}
                  </span>
                ) : view.javaDetected ? (
                  <>
                    Not set. Found <span className="mono">{shownPath(view.javaDetected)}</span>.
                  </>
                ) : (
                  "Not set. Version 21 or newer, needed by Ghidra."
                )
              }
            >
              {!view.javaHome && view.javaDetected && (
                <Button size="sm" onClick={() => void set("detectJava")} title={shownPath(view.javaDetected)}>
                  Use this one
                </Button>
              )}
              <Button size="sm" onClick={() => void set("java")}>
                Choose folder…
              </Button>
            </SettingRow>
            <SettingRow
              label="Decompiling"
              help={
                view.ready
                  ? "The agent can decompile with Ghidra."
                  : view.problem ?? "Off until both folders are set."
              }
            >
              {view.ready ? <Pill tone="ok">Ready</Pill> : view.problem ? <Pill tone="warn">Needs attention</Pill> : <Pill tone="neutral">Off</Pill>}
            </SettingRow>
          </SettingGroup>
          {(view.ghidraDir || view.javaHome) && (
            <Button size="sm" variant="ghost" className="gw-danger-text" onClick={() => void set("clear")}>
              Remove Ghidra setup
            </Button>
          )}
        </>
      )}
    </section>
  );
}

// Personal skills: SKILL.md files the agent can use in every session.
// Project skills live in a granted folder (.gardenwall/skills/<name>/SKILL.md
// or .claude/skills/<name>/SKILL.md) and need no setup here.
import { useEffect, useState } from "react";
import { Plus } from "lucide-react";
import { Button, Fold, SettingGroup, SettingRow } from "./ui";
import { api, errorText, SkillView } from "./api";

export function SkillsSettings() {
  const [skills, setSkills] = useState<SkillView[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    api.listSkills().then(setSkills).catch((e) => setError(errorText(e)));
  }, []);
  const act = async (f: () => Promise<SkillView[]>) => {
    setError(null);
    try {
      setSkills(await f());
    } catch (e) {
      setError(errorText(e));
    }
  };
  return (
    <section>
      <h2>Skills</h2>
      <Fold summary="Instructions the agent loads when a task matches. They only instruct: every action still needs your permissions.">
        A skill is a folder with a SKILL.md file: a name and description at the top, then the instructions. The agent
        sees each description and loads the instructions when a task matches. Personal skills added here apply to every
        session; project skills in a granted folder's .gardenwall/skills or .claude/skills apply to sessions on that
        folder.
      </Fold>
      {error && <div className="banner error">{error}</div>}
      <SettingGroup>
        {skills && skills.length === 0 && (
          <SettingRow label="No personal skills yet" help="Add a skill folder to use it in every session.">
            <span />
          </SettingRow>
        )}
        {skills?.map((s) => (
          <SettingRow
            key={s.name}
            label={<span className="mono">{s.name}</span>}
            help={
              <>
                {s.description}
                {s.files ? ` · ${s.files} bundled file${s.files === 1 ? "" : "s"}` : ""}
              </>
            }
          >
            <Button size="sm" variant="ghost" onClick={() => void act(() => api.removeSkill(s.name))}>
              Remove
            </Button>
          </SettingRow>
        ))}
      </SettingGroup>
      <Button icon={Plus} onClick={() => void act(() => api.addSkill())}>
        Add skill folder…
      </Button>
    </section>
  );
}

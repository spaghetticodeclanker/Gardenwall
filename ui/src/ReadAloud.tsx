// The Read aloud button on a reply (release plan feature 8): Read aloud,
// then Stop while that reply plays. An error shows once, as a toast, from
// the button that started it.
import { useEffect } from "react";
import { Loader2, Square, Volume2 } from "lucide-react";
import { clearSpeechError, readAloud, speechKey, stopReading, useSpeech } from "./speech";
import { IconButton, useToast } from "./ui";

export function ReadAloudButton({ text }: { text: string }) {
  const speech = useSpeech();
  const toast = useToast();
  const key = speechKey(text);
  const mine = speech.key === key;
  useEffect(() => {
    if (speech.error && speech.errorKey === key) {
      toast({ text: `Couldn't read aloud: ${speech.error}`, tone: "danger" });
      clearSpeechError();
    }
  }, [speech.error, speech.errorKey, key, toast]);
  if (mine && speech.loading) return <IconButton icon={Loader2} label="Preparing to read aloud… (click to cancel)" onClick={stopReading} />;
  if (mine) return <IconButton icon={Square} label="Stop reading" onClick={stopReading} />;
  return <IconButton icon={Volume2} label="Read aloud (code and tool output are announced, not read)" onClick={() => void readAloud(text)} />;
}

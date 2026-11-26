// Shown when replies became much slower, or the GPU's memory is nearly
// full. The usual cause on Windows: another app took GPU memory after the
// model loaded, and the driver moved part of the model into much slower
// system memory. Process names come from the trusted core's OS query.
import { formatBytes, GpuWarning } from "./api";

export function SlowdownBanner({
  warning,
  goToModels,
  onDismiss,
}: {
  warning: GpuWarning;
  goToModels: () => void;
  onDismiss: () => void;
}) {
  const gpu = warning.gpu;
  const apps = gpu?.apps ?? [];
  const headline =
    warning.kind === "slowdown" && warning.baseline != null && warning.current != null
      ? `Generation slowed from ${warning.baseline.toFixed(0)} to ${warning.current.toFixed(0)} tokens per second.`
      : gpu && gpu.usedBytes != null
        ? `Your GPU's memory is almost full (${formatBytes(gpu.usedBytes)} of ${formatBytes(gpu.totalBytes)} in use).`
        : "Your GPU's memory is almost full.";
  const why =
    warning.kind === "slowdown"
      ? "This usually means another app is now using GPU memory, so part of the model was moved to slower system memory."
      : "If other apps take more, part of the model will be moved to system memory and replies will get several times slower.";
  return (
    <div className="banner warn" role="status">
      <strong>{headline}</strong> {why}
      {gpu && gpu.modelBytes != null && <> The model uses {formatBytes(gpu.modelBytes)}.</>}
      {apps.length > 0 ? (
        <>
          {" "}
          Apps using the most GPU memory:{" "}
          {apps.map((a, i) => (
            <span key={a.name + i}>
              {i > 0 && ", "}
              <span className="mono">{a.name}</span> ({formatBytes(a.bytes)})
            </span>
          ))}
          . Closing some of them, then reloading the model, gets full speed back.
        </>
      ) : (
        <>
          {" "}
          To get full speed back, close GPU-heavy apps (games, video or 3D editors, other AI tools, browser tabs playing
          video), then reload the model.
        </>
      )}{" "}
      You can also reload with a smaller context size or the q8_0 KV cache, which needs less GPU memory.{" "}
      <button className="link" onClick={goToModels}>
        Go to Models
      </button>{" "}
      ·{" "}
      <button className="link" onClick={onDismiss}>
        Dismiss
      </button>
    </div>
  );
}

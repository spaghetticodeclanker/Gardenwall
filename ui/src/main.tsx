import { Component, ErrorInfo, ReactNode, StrictMode } from "react";
import { createRoot } from "react-dom/client";
// Base styles first, so the component styles (ui/, shell/) come later in the cascade.
import "./fonts";
import "./styles.css";
import { App } from "./App";

/** Shows a recoverable message instead of a blank window if the UI crashes. */
class ErrorBoundary extends Component<{ children: ReactNode }, { error: Error | null }> {
  state = { error: null as Error | null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error("Gardenwall UI error:", error, info.componentStack);
  }

  render() {
    if (!this.state.error) return this.props.children;
    return (
      <div className="crash">
        <h2>Something went wrong in the Gardenwall window</h2>
        <p className="small muted">
          The app itself is still running; anything the agent was doing continues or stops safely in the background.
        </p>
        <pre className="tool-pre">{String(this.state.error.message || this.state.error)}</pre>
        <button className="primary" onClick={() => location.reload()}>
          Reload the window
        </button>
      </div>
    );
  }
}

const root = document.getElementById("root");
if (root) {
  createRoot(root).render(
    <StrictMode>
      <ErrorBoundary>
        <App />
      </ErrorBoundary>
    </StrictMode>,
  );
}

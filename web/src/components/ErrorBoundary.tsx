import { Component } from "react";
import type { ErrorInfo, ReactNode } from "react";

interface Props {
  children: ReactNode;
}

interface State {
  error: Error | null;
}

// Top-level boundary: a render error anywhere below becomes a readable message with a reload,
// instead of a blank white window in the packaged app (which has no address bar or devtools to
// recover from). Reload re-navigates the current token URL the shell handed us.
export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    console.error("Unhandled render error:", error, info.componentStack);
  }

  render(): ReactNode {
    if (this.state.error) {
      return (
        <div className="crash" role="alert">
          <h1 className="crash__title">Something went wrong</h1>
          <p className="crash__message">{this.state.error.message}</p>
          <button type="button" className="crash__reload" onClick={() => window.location.reload()}>
            Reload
          </button>
        </div>
      );
    }
    return this.props.children;
  }
}

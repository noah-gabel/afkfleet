// The app's outermost error boundary (Plan.md P8.6): a render error anywhere, even outside
// the router, shows the error screen instead of a blank window, and goes to the console
// (the debug build's devtools). React still needs a class component for this.
import { Component, type ErrorInfo, type ReactNode } from "react";

import { ErrorPanel } from "./ErrorPanel";

interface Props {
  readonly children: ReactNode;
  /** Replaces the error screen's reload (tests). */
  readonly onRestart?: () => void;
}

type State = { readonly failed: false } | { readonly failed: true; readonly error: unknown };

/** The error boundary around the whole app. */
export class AppErrorBoundary extends Component<Props, State> {
  override state: State = { failed: false };

  static getDerivedStateFromError(error: unknown): State {
    return { failed: true, error };
  }

  override componentDidCatch(error: unknown, _info: ErrorInfo): void {
    console.error(error);
  }

  override render(): ReactNode {
    if (!this.state.failed) {
      return this.props.children;
    }
    const { onRestart } = this.props;
    return (
      <ErrorPanel
        error={this.state.error}
        onReset={() => this.setState({ failed: false })}
        {...(onRestart === undefined ? {} : { onRestart })}
      />
    );
  }
}

import { lazy, Suspense, useState } from "react";

import { useMeetings } from "./api/hooks";
import type { MeetingRead } from "./api/types";
import { MeetingList } from "./components/MeetingList";
import { TranscriptView } from "./components/TranscriptView";

// Route-level code splitting: the settings page loads only when opened.
const SettingsPage = lazy(() => import("./components/SettingsPage"));

export function App() {
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [showSettings, setShowSettings] = useState(false);
  const meetings = useMeetings();
  const items = meetings.data?.items ?? [];
  const selected: MeetingRead | null = items.find((m) => m.id === selectedId) ?? null;

  return (
    <div className="app">
      <header className="app__bar">
        <h1 className="app__title">hearsay - It's what happened, probably</h1>
        <button type="button" className="app__settings" onClick={() => setShowSettings(true)}>
          Settings
        </button>
      </header>
      <main className="app__main">
        <MeetingList
          meetings={items}
          isLoading={meetings.isLoading}
          error={meetings.error}
          selectedId={selectedId}
          onSelect={setSelectedId}
        />
        <TranscriptView meeting={selected} />
      </main>
      {showSettings ? (
        <Suspense fallback={null}>
          <SettingsPage onClose={() => setShowSettings(false)} />
        </Suspense>
      ) : null}
    </div>
  );
}

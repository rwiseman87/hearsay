import { useState } from "react";

import { useMeetings } from "./api/hooks";
import type { MeetingRead } from "./api/types";
import { MeetingList } from "./components/MeetingList";
import { ModelPicker } from "./components/ModelPicker";
import { TranscriptView } from "./components/TranscriptView";

export function App() {
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const meetings = useMeetings();
  const items = meetings.data?.items ?? [];
  const selected: MeetingRead | null = items.find((m) => m.id === selectedId) ?? null;

  return (
    <div className="app">
      <header className="app__bar">
        <h1 className="app__title">hearsay</h1>
        <ModelPicker />
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
    </div>
  );
}

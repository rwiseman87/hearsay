import { useGenerateNotes, useMeetingNotes, useSettings } from "../api/hooks";

interface Props {
  meetingId: string;
  recording: boolean;
}

// Post-meeting notes: a local-LLM summary + action items over the finalized transcript. Shows the
// stored notes when present, a Generate/Regenerate action, and points at Settings when no
// summarization model is configured yet. Hidden while a meeting is still recording (the transcript
// is not final, so there is nothing to summarize).
export function NotesPanel({ meetingId, recording }: Props) {
  // Skip the fetch while recording — its 404 empty-state would be meaningless mid-meeting.
  const notes = useMeetingNotes(recording ? null : meetingId);
  const generate = useGenerateNotes(meetingId);
  const settings = useSettings();
  const modelReady = settings.data?.models_info.notes_model_exists ?? false;
  const data = notes.data;

  if (recording) return null;

  const busy = generate.isPending;
  const label = busy ? "Generating…" : data ? "Regenerate" : "Generate notes";

  return (
    <section className="notes">
      <div className="notes__head">
        <h3 className="notes__title">Notes</h3>
        <button
          type="button"
          onClick={() => generate.mutate()}
          disabled={busy || !modelReady}
          title={modelReady ? undefined : "Choose a summarization model in Settings › Models"}
        >
          {label}
        </button>
      </div>

      {data ? (
        <div className="notes__body">
          <h4 className="notes__subhead">Summary</h4>
          <p className="notes__summary">{data.summary}</p>
          {data.action_items.length > 0 ? (
            <>
              <h4 className="notes__subhead">Action items</h4>
              <ul className="notes__actions">
                {data.action_items.map((item, index) => (
                  <li key={index}>{item}</li>
                ))}
              </ul>
            </>
          ) : null}
          <p className="notes__meta muted">Generated locally by {data.model}</p>
        </div>
      ) : !modelReady ? (
        <p className="muted notes__hint">
          Choose a summarization model in Settings › Models to generate a summary and action items.
        </p>
      ) : (
        <p className="muted notes__hint">
          No notes yet — click Generate to summarize this meeting and pull out action items.
        </p>
      )}

      {generate.isError ? (
        <p className="notes__error" role="alert">
          {(generate.error as Error).message}
        </p>
      ) : null}
    </section>
  );
}

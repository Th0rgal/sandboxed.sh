import { Suspense } from "react";
import ConversationRouter from "./conversation-router";

export default function ControlPage() {
  // No visible Suspense fallback: AuthGate's full-screen ring covers the cold
  // load, and the conversation body renders its own skeleton inside the chat
  // area while data fetches. A centered icon here only flashes for a few
  // hundred ms between the two and reads as an extra unrelated spinner.
  return (
    <Suspense fallback={null}>
      <ConversationRouter />
    </Suspense>
  );
}

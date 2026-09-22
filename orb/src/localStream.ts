/** Coalesce transport fragments into one paint without delaying terminal state. */
export interface OutputEvent<T> {
  text: string;
  reset: boolean;
  state?: T | null;
}
export function bufferedOutput<T extends { text: string }>(
  paint: (text: string) => void,
  complete: (state: T) => void,
) {
  let text = "",
    shown = "",
    timer: ReturnType<typeof setTimeout> | undefined,
    finished = false;
  const flush = () => {
    if (timer !== undefined) clearTimeout(timer);
    timer = undefined;
    if (text !== shown) {
      shown = text;
      paint(text);
    }
  };
  return {
    receive(event: OutputEvent<T>) {
      if (finished) return;
      text = event.reset ? event.text : text + event.text;
      if (event.state) {
        text = event.state.text;
        finished = true;
        flush();
        complete(event.state);
      } else if (timer === undefined) timer = setTimeout(flush, 16);
    },
    dispose() {
      finished = true;
      if (timer !== undefined) clearTimeout(timer);
    },
  };
}

"use client";

import {
  type ButtonHTMLAttributes,
  type MouseEvent,
  type ReactNode,
  forwardRef,
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";
import { Loader } from "lucide-react";
import { cn } from "@/lib/utils";

type AsyncClickHandler = (
  event: MouseEvent<HTMLButtonElement>
) => void | Promise<unknown>;

type AsyncButtonProps = Omit<
  ButtonHTMLAttributes<HTMLButtonElement>,
  "onClick"
> & {
  onClick?: AsyncClickHandler;
  /** Externally-controlled busy state (overrides internal tracking). */
  busy?: boolean;
  /** Optional text shown next to the spinner while busy (e.g. "Saving…"). */
  busyText?: ReactNode;
  /** Override the spinner element entirely. */
  spinner?: ReactNode;
  spinnerClassName?: string;
};

/**
 * A button that automatically tracks the pending state of an async onClick
 * handler. While the returned promise is in flight the button is disabled,
 * the children are visually hidden (layout preserved), and a spinner is
 * overlaid. Consumers can also pass `busy` to drive the state externally.
 */
export const AsyncButton = forwardRef<HTMLButtonElement, AsyncButtonProps>(
  function AsyncButton(
    {
      onClick,
      busy,
      busyText,
      spinner,
      spinnerClassName,
      className,
      children,
      disabled,
      type,
      ...rest
    },
    ref
  ) {
    const [internalBusy, setInternalBusy] = useState(false);
    const inFlight = useRef(false);
    const mounted = useRef(true);

    useEffect(
      () => () => {
        mounted.current = false;
      },
      []
    );

    const isBusy = Boolean(busy) || internalBusy;

    const handleClick = useCallback(
      async (event: MouseEvent<HTMLButtonElement>) => {
        if (!onClick || inFlight.current) return;
        const result = onClick(event);
        if (!(result && typeof (result as Promise<unknown>).then === "function")) {
          return;
        }
        inFlight.current = true;
        setInternalBusy(true);
        try {
          await result;
        } finally {
          inFlight.current = false;
          if (mounted.current) setInternalBusy(false);
        }
      },
      [onClick]
    );

    const spinnerNode = spinner ?? (
      <Loader
        className={cn("h-3.5 w-3.5 animate-spin", spinnerClassName)}
        aria-hidden
      />
    );

    return (
      <button
        ref={ref}
        type={type ?? "button"}
        {...rest}
        disabled={disabled || isBusy}
        aria-busy={isBusy || undefined}
        data-busy={isBusy ? "true" : undefined}
        onClick={handleClick}
        className={cn("relative", className)}
      >
        <span
          className={cn(
            "inline-flex items-center justify-center gap-1.5 transition-opacity",
            isBusy && "invisible"
          )}
        >
          {children}
        </span>
        {isBusy && (
          <span className="pointer-events-none absolute inset-0 flex items-center justify-center">
            {busyText ? (
              <span className="inline-flex items-center gap-1.5">
                {spinnerNode}
                <span>{busyText}</span>
              </span>
            ) : (
              spinnerNode
            )}
          </span>
        )}
      </button>
    );
  }
);

/**
 * Imperative variant for cases where you can't use AsyncButton (custom
 * triggers, anchor tags, popover items, etc.). Returns a guarded `run`
 * that ignores re-entry while the previous call is in flight, plus the
 * `pending` flag and a `isSlow` flag that flips true after `slowAfterMs`.
 */
export function useAsyncAction<TArgs extends unknown[]>(
  handler: (...args: TArgs) => unknown,
  options: { slowAfterMs?: number } = {}
) {
  const { slowAfterMs = 800 } = options;
  const [pending, setPending] = useState(false);
  const [isSlow, setIsSlow] = useState(false);
  const inFlight = useRef(false);
  const mounted = useRef(true);
  const handlerRef = useRef(handler);

  useEffect(() => {
    handlerRef.current = handler;
  }, [handler]);

  useEffect(
    () => () => {
      mounted.current = false;
    },
    []
  );

  const run = useCallback(
    async (...args: TArgs) => {
      if (inFlight.current) return;
      const result = handlerRef.current(...args);
      if (!(result && typeof (result as Promise<unknown>).then === "function")) {
        return;
      }
      inFlight.current = true;
      setPending(true);
      setIsSlow(false);
      const slowTimer = window.setTimeout(() => {
        if (mounted.current) setIsSlow(true);
      }, slowAfterMs);
      try {
        await result;
      } finally {
        clearTimeout(slowTimer);
        inFlight.current = false;
        if (mounted.current) {
          setPending(false);
          setIsSlow(false);
        }
      }
    },
    [slowAfterMs]
  );

  return { run, pending, isSlow };
}

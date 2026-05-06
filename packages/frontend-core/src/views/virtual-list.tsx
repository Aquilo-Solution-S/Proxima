import {
  For,
  createMemo,
  createSignal,
  onCleanup,
  onMount,
  type JSX,
} from "solid-js";

type VirtualEntry<T> = {
  item: T;
  index: number;
  key: string;
  top: number;
};

const DEFAULT_VIEWPORT_HEIGHT = 640;

const VirtualRow = <T,>(props: {
  entry: VirtualEntry<T>;
  onMeasure: (key: string, height: number) => void;
  children: (item: T, index: number) => JSX.Element;
}) => {
  let rowRef!: HTMLDivElement;
  let ro: ResizeObserver | null = null;

  const measure = () => {
    const height = rowRef.getBoundingClientRect().height;
    if (height > 0) props.onMeasure(props.entry.key, height);
  };

  onMount(() => {
    measure();
    if (typeof ResizeObserver === "undefined") return;
    ro = new ResizeObserver((entries) => {
      const height = entries[0]?.contentRect.height ?? 0;
      if (height > 0) props.onMeasure(props.entry.key, height);
    });
    ro.observe(rowRef);
  });

  onCleanup(() => ro?.disconnect());

  return (
    <div
      ref={rowRef}
      class="virtual-list-row"
      style={{ transform: `translateY(${props.entry.top}px)` }}
    >
      {props.children(props.entry.item, props.entry.index)}
    </div>
  );
};

export const VirtualList = <T,>(props: {
  items: readonly T[];
  itemKey: (item: T, index: number) => string;
  estimateSize: number;
  overscan?: number;
  class?: string;
  role?: JSX.HTMLAttributes<HTMLDivElement>["role"];
  ariaLabel?: string;
  gap?: number;
  children: (item: T, index: number) => JSX.Element;
}) => {
  let scrollerRef!: HTMLDivElement;
  let ro: ResizeObserver | null = null;
  const [scrollTop, setScrollTop] = createSignal(0);
  const [viewportHeight, setViewportHeight] = createSignal(
    DEFAULT_VIEWPORT_HEIGHT,
  );
  const [sizes, setSizes] = createSignal(new Map<string, number>());

  const updateViewport = () => {
    const height = scrollerRef.clientHeight;
    if (height > 0) setViewportHeight(height);
  };

  const updateSize = (key: string, height: number) => {
    setSizes((current) => {
      if (current.get(key) === height) return current;
      const next = new Map(current);
      next.set(key, height);
      return next;
    });
  };

  const layout = createMemo(() => {
    const measured = sizes();
    const positions: number[] = [];
    let total = 0;
    props.items.forEach((item, index) => {
      positions[index] = total;
      total += measured.get(props.itemKey(item, index)) ?? props.estimateSize;
    });
    return { positions, total };
  });

  const visible = createMemo(() => {
    const items = props.items;
    const { positions, total } = layout();
    if (items.length === 0) return [];

    const estimate = Math.max(1, props.estimateSize);
    const overscan = props.overscan ?? 8;
    const startEdge = Math.max(0, scrollTop() - overscan * estimate);
    const endEdge =
      scrollTop() + viewportHeight() + overscan * estimate;

    let start = 0;
    while (
      start < items.length - 1 &&
      (positions[start + 1] ?? total) < startEdge
    ) {
      start++;
    }

    let end = start;
    while (end < items.length && positions[end] <= endEdge) {
      end++;
    }

    const entries: VirtualEntry<T>[] = [];
    for (let index = start; index < end; index++) {
      const item = items[index];
      if (item === undefined) continue;
      entries.push({
        item,
        index,
        key: props.itemKey(item, index),
        top: positions[index] ?? 0,
      });
    }
    return entries;
  });

  onMount(() => {
    updateViewport();
    if (typeof ResizeObserver === "undefined") return;
    ro = new ResizeObserver(updateViewport);
    ro.observe(scrollerRef);
  });

  onCleanup(() => ro?.disconnect());

  return (
    <div
      ref={scrollerRef}
      class={`virtual-list${props.class ? ` ${props.class}` : ""}`}
      role={props.role}
      aria-label={props.ariaLabel}
      style={{ "--virtual-list-gap": `${props.gap ?? 0}px` }}
      onScroll={(event) => setScrollTop(event.currentTarget.scrollTop)}
    >
      <div
        class="virtual-list-spacer"
        style={{ height: `${layout().total}px` }}
      >
        <For each={visible()}>
          {(entry) => (
            <VirtualRow entry={entry} onMeasure={updateSize}>
              {props.children}
            </VirtualRow>
          )}
        </For>
      </div>
    </div>
  );
};

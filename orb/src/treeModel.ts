/** One hierarchy for all sidebar rows. Visibility and rails never depend on DOM siblings. */
export interface TreeNode<T> {
  id: string;
  data: T;
  expanded?: boolean;
  children?: TreeNode<T>[];
}
export interface TreeRow<T> {
  id: string;
  parentId?: string;
  data: T;
  depth: number;
  position: number;
  size: number;
  following: boolean;
  /** Columns of ancestors whose following siblings still need a vertical rail. */
  continuations: number[];
  expanded?: boolean;
  connectsChildren: boolean;
}
export function visibleTree<T>(nodes: TreeNode<T>[]): TreeRow<T>[] {
  const rows: TreeRow<T>[] = [];
  const visit = (siblings: TreeNode<T>[], depth: number, continuations: number[], parentId?: string) => {
    siblings.forEach((node, index) => {
      const following = index < siblings.length - 1;
      const connectsChildren = node.expanded === true && !!node.children?.length;
      rows.push({ id: node.id, parentId, data: node.data, depth, position: index + 1,
        size: siblings.length, following, continuations, expanded: node.expanded, connectsChildren });
      if (connectsChildren) visit(node.children!, depth + 1,
        depth > 0 && following ? [...continuations, depth - 1] : continuations, node.id);
    });
  };
  visit(nodes, 0, []);
  return rows;
}

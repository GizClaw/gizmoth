export interface ProjectFile {
  path: string;
}
export interface ProjectTreeNode {
  name: string;
  path: string;
  children?: ProjectTreeNode[];
}
export function projectPathSegments(path: string) {
  const parts = path ? path.split("/") : [];
  return parts.map((name, index) => ({
    name,
    path: parts.slice(0, index + 1).join("/"),
    parent: parts.slice(0, index).join("/"),
    file: index === parts.length - 1,
  }));
}

export function projectTreeChildren(tree: ProjectTreeNode[], parent: string): ProjectTreeNode[] {
  if (!parent) return tree;
  let nodes = tree;
  for (const segment of parent.split("/")) {
    const folder = nodes.find((node) => node.name === segment && node.children);
    if (!folder) return [];
    nodes = folder.children!;
  }
  return nodes;
}

export function visibleProjectTree(
  nodes: ProjectTreeNode[],
  expanded: ReadonlySet<string>,
  depth = 0,
): { node: ProjectTreeNode; depth: number }[] {
  return nodes.flatMap((node) => [
    { node, depth },
    ...(node.children && expanded.has(node.path) ? visibleProjectTree(node.children, expanded, depth + 1) : []),
  ]);
}

export function projectFileTree(files: ProjectFile[]): ProjectTreeNode[] {
  const root: ProjectTreeNode[] = [];
  for (const file of files) {
    const parts = file.path.split("/");
    let nodes = root;
    for (let index = 0; index < parts.length; index++) {
      const path = parts.slice(0, index + 1).join("/");
      let node = nodes.find((item) => item.path === path);
      if (!node) {
        node = { name: parts[index], path, ...(index < parts.length - 1 ? { children: [] } : {}) };
        nodes.push(node);
      }
      if (node.children) nodes = node.children;
    }
  }
  const order = (nodes: ProjectTreeNode[]) => {
    nodes.sort((a, b) => Number(!!b.children) - Number(!!a.children) || a.name.localeCompare(b.name));
    nodes.forEach((node) => {
      if (node.children) order(node.children);
    });
  };
  order(root);
  return root;
}

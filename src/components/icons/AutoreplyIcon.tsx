import { createLucideIcon } from "lucide-react";

/** A comment bubble with two sparks: Chatelle's Autoreply button. Drawn on
 * Lucide's grid and stroke so it matches the other icons. */
const AutoreplyIcon = createLucideIcon("autoreply", [
  [
    "path",
    {
      d: "M17 13v5a2 2 0 0 1-2 2H6.828a2 2 0 0 0-1.414.586l-2.202 2.202A.71.71 0 0 1 2 22.286V9a2 2 0 0 1 2-2h7",
      key: "bubble",
    },
  ],
  ["path", { d: "M6 12h5", key: "line1" }],
  ["path", { d: "M6 16h7", key: "line2" }],
  [
    "path",
    {
      d: "M18.5 2.5q.6 3.4 4 4q-3.4.6-4 4q-.6-3.4-4-4q3.4-.6 4-4z",
      key: "spark1",
    },
  ],
  [
    "path",
    {
      d: "M12.5 1.5q.25 1.25 1.5 1.5q-1.25.25-1.5 1.5q-.25-1.25-1.5-1.5q1.25-.25 1.5-1.5z",
      key: "spark2",
    },
  ],
]);

export default AutoreplyIcon;

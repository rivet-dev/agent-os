import map from "lodash-es/map.js";
import filter from "lodash-es/filter.js";
import groupBy from "lodash-es/groupBy.js";
import debounce from "lodash-es/debounce.js";
import sortBy from "lodash-es/sortBy.js";
import uniq from "lodash-es/uniq.js";

const items = [
	{ name: "Alice", group: "A", score: 90 },
	{ name: "Bob", group: "B", score: 85 },
	{ name: "Carol", group: "A", score: 95 },
	{ name: "Dave", group: "B", score: 80 },
];

const names = map(items, "name");
const highScores = filter(items, (i) => i.score >= 90);
const grouped = groupBy(items, "group");
const sorted = sortBy(items, "score").map((i) => i.name);
const unique = uniq([1, 2, 2, 3, 3, 3]);

const result = {
	names,
	highScoreNames: map(highScores, "name"),
	groupKeys: Object.keys(grouped).sort(),
	groupACount: grouped["A"].length,
	sorted,
	unique,
	debounceType: typeof debounce,
};

console.log(JSON.stringify(result));

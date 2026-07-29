export default {
  test: {
    include: ["src/**/*.test.js"],
    pool: "forks",
    maxWorkers: 1,
    fileParallelism: false,
  },
};

Object.defineProperty(exports, "__esModule", { value: true });

exports.default = function handler(req, res) {
	res.status(200).json({ message: "hello", method: req.method });
};

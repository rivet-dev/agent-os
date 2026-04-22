"""Pure-Python shim for dbt-extractor.

Raises ExtractionError so dbt-core falls back to full Jinja rendering.
See registry/python-wheels/recipes/dbt-extractor-shim/README.md.
"""

__version__ = "0.6.0+pyodide.shim"


class ExtractionError(Exception):
    """Raised when the static parser is unavailable.

    dbt-core's `ModelParser.run_static_parser` catches this and falls back
    to the full Jinja parsing path. See dbt/parser/models.py:370-373.
    """


def py_extract_from_source(source: str) -> dict:
    """Static parser entry point — always raises ExtractionError in the shim.

    The real Rust implementation parses Jinja blocks and returns a dict
    of refs/sources/configs. The shim deliberately fails so dbt's fallback
    path takes over.
    """
    raise ExtractionError(
        "static parser disabled in pyodide; falling back to full Jinja"
    )


__all__ = ["ExtractionError", "py_extract_from_source", "__version__"]

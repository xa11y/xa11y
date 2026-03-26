project = "xa11y"
copyright = "2025, xa11y contributors"
author = "xa11y contributors"

extensions = [
    "autoapi.extension",
]

# sphinx-autoapi: generate docs from .pyi stubs without importing the module
autoapi_type = "python"
autoapi_dirs = ["../../xa11y-python/python"]
autoapi_options = [
    "members",
    "show-inheritance",
    "show-module-summary",
    "imported-members",
]
autoapi_ignore = ["*/_native.abi3*", "*/py.typed"]
autoapi_python_class_content = "both"
autoapi_member_order = "groupwise"
autoapi_keep_files = False
autoapi_root = "api"
autoapi_add_toctree_entry = False

# Exclude private/test helpers and the internal _native submodule page
autoapi_python_use_implicit_namespaces = False


def autoapi_skip_member(app, what, name, obj, skip, options):
    # Skip private/test helpers
    if name.startswith("_"):
        return True
    return skip


def setup(app):
    app.connect("autoapi-skip-member", autoapi_skip_member)

# Theme
html_theme = "sphinx_rtd_theme"
html_theme_options = {
    "navigation_depth": 3,
    "collapse_navigation": False,
}

# Don't show "Created using Sphinx" in footer
html_show_sphinx = False
html_show_sourcelink = False

# Suppress the module index (small API, not needed)
html_domain_indices = False

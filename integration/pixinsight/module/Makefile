# Convenience wrapper: delegate to the platform makefile.
# Overridable vars (PCLINCDIR, PCLLIBDIR; and for `sign`: XSSK_FILE,
# XSSK_PASSWORD, PIXINSIGHT, SIGN_EXIT) pass straight through.

.PHONY: all clean sign
all clean sign:
	$(MAKE) -f makefile-x64 $@

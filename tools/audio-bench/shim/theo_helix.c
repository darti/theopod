/* theo-pod bench shim (t-164): state size for AACInitDecoderPre, no malloc. */
#include "aaccommon.h"
#include "coder.h"

int theo_helix_aac_state_size(void)
{
	return (int)(((sizeof(AACDecInfo) + 7) & ~7) + ((sizeof(PSInfoBase) + 7) & ~7));
}

/* The bench passes its own state buffer: these must never be called. */
void *helix_malloc(int size)
{
	(void)size;
	return 0;
}

void helix_free(void *ptr)
{
	(void)ptr;
}

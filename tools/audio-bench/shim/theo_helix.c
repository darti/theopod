/* theo-pod bench shim (t-164): AAC state size for AACInitDecoderPre, and a
 * bump arena the bench hands to Helix MP3 (MP3InitDecoder allocates). */
#include "aaccommon.h"
#include "coder.h"

int theo_helix_aac_state_size(void)
{
	return (int)(((sizeof(AACDecInfo) + 7) & ~7) + ((sizeof(PSInfoBase) + 7) & ~7));
}

static char *arena;
static int arena_left;
static int arena_used;

/* Hands Helix the next allocations; AAC uses AACInitDecoderPre instead. */
void theo_helix_arena(void *ptr, int size)
{
	arena = (char *)ptr;
	arena_left = size;
	arena_used = 0;
}

int theo_helix_arena_used(void)
{
	return arena_used;
}

void *helix_malloc(int size)
{
	int need = (size + 7) & ~7;
	void *block;

	if (!arena || need > arena_left)
		return 0;
	block = arena;
	arena += need;
	arena_left -= need;
	arena_used += need;
	return block;
}

/* The arena is released as a whole by the bench. */
void helix_free(void *ptr)
{
	(void)ptr;
}

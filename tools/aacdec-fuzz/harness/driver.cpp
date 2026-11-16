/* theo-pod opencore-aacdec fuzz driver (t-169): one input, one decoder.
 *
 * Drives the decoder only through the bench shim (theo_opencore.cpp), the
 * way the firmware does: zeroed state of theo_opencore_size() bytes, then
 * theo_opencore_init, then theo_opencore_decode once per access unit, going
 * on after errors. Every buffer is an exact-size heap copy, so ASan reports
 * any byte read or written past the input, the state or the PCM output.
 *
 * Input layout (byte 0 selects the path):
 *   even: raw     [0] [asc_len] [asc ...] { [len lo] [len hi] [frame ...] }*
 *         an access unit cut short by the end of the input keeps the bytes
 *         that remain (a truncated download);
 *   odd:  ADTS    [1] [stream ...]: init without an AudioSpecificConfig (the
 *         decoder then probes ADIF, else ADTS, on the first frame), then
 *         decode from the current position, advancing by the bytes used.
 */
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

extern "C" unsigned theo_opencore_size(void);
extern "C" int theo_opencore_init(void *mem, const unsigned char *asc, int asc_len);
extern "C" int theo_opencore_decode(void *mem, const unsigned char *frame, int len,
				    short *pcm, int *used);

/* What one input did, for the host regression tests (decode.rs). */
struct theo_fuzz_summary {
	int init;	/* theo_opencore_init's error code (0: config parsed) */
	int frames;	/* theo_opencore_decode calls */
	int decoded;	/* calls that returned MP4AUDEC_SUCCESS (0) */
};

/* Shim contract: 2048 core samples plus 2048 for the SBR output. */
static const size_t PCM_SAMPLES = 4096;
/* Bounds one input's run time: about 1 s of 48 kHz audio. */
static const int MAX_FRAMES = 64;

static unsigned char *copy(const uint8_t *data, size_t len)
{
	/* malloc(0) may return NULL: keep one valid, unreadable-past-end block. */
	unsigned char *buf = (unsigned char *)malloc(len ? len : 1);
	if (buf && len)
		memcpy(buf, data, len);
	return buf;
}

static int decode(void *mem, const uint8_t *data, size_t len, short *pcm, int *used,
		  struct theo_fuzz_summary *summary)
{
	unsigned char *frame = copy(data, len);
	if (!frame)
		return -1;
	*used = 0;
	int err = theo_opencore_decode(mem, frame, (int)len, pcm, used);
	free(frame);
	summary->frames++;
	summary->decoded += err == 0;
	return err;
}

static void raw(void *mem, const uint8_t *data, size_t size, short *pcm,
		struct theo_fuzz_summary *summary)
{
	if (size < 1)
		return;
	size_t asc_len = data[0];
	data++, size--;
	if (asc_len > size)
		asc_len = size;
	unsigned char *asc = copy(data, asc_len);
	if (!asc)
		return;
	summary->init = theo_opencore_init(mem, asc, (int)asc_len);
	free(asc);
	data += asc_len, size -= asc_len;
	for (int frames = 0; frames < MAX_FRAMES && size >= 2; frames++) {
		size_t len = data[0] | (size_t)data[1] << 8;
		data += 2, size -= 2;
		if (len > size)
			len = size;
		int used;
		decode(mem, data, len, pcm, &used, summary);
		data += len, size -= len;
	}
}

static void adts(void *mem, const uint8_t *data, size_t size, short *pcm,
		 struct theo_fuzz_summary *summary)
{
	/* An empty, non-null config: the decoder still computes a read
	 * pointer from it, and a null base would be UB of the driver's own. */
	unsigned char *none = copy(data, 0);
	if (!none)
		return;
	summary->init = theo_opencore_init(mem, none, 0);
	free(none);
	for (int frames = 0; frames < MAX_FRAMES && size > 0; frames++) {
		int used;
		decode(mem, data, size, pcm, &used, summary);
		/* No progress, or a used count outside the input: stop, as a
		 * player that resynchronizes on the next buffer would. */
		if (used <= 0 || (size_t)used > size)
			break;
		data += used, size -= used;
	}
}

extern "C" struct theo_fuzz_summary theo_fuzz_one(const uint8_t *data, size_t size)
{
	struct theo_fuzz_summary summary = {-1, 0, 0};
	if (size < 1)
		return summary;
	void *mem = calloc(1, theo_opencore_size());
	short *pcm = (short *)calloc(PCM_SAMPLES, sizeof(short));
	if (mem && pcm) {
		if (data[0] & 1)
			adts(mem, data + 1, size - 1, pcm, &summary);
		else
			raw(mem, data + 1, size - 1, pcm, &summary);
	}
	free(pcm);
	free(mem);
	return summary;
}

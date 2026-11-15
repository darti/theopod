/* theo-pod bench shim (t-167): a C API over the PacketVideo AAC decoder
 * (opencore-aacdec, Apache-2.0), driven the way AOSP's SoftAAC.cpp drives it.
 * One decoder at a time: the external structure is a static here, the
 * decoder state is the caller's `mem` (theo_opencore_size() bytes). */
#include <string.h>

#include "e_tmp4audioobjecttype.h"
#include "pvmp4audiodecoder_api.h"

static tPVMP4AudioDecoderExternal ext;

extern "C" unsigned theo_opencore_size(void)
{
	return PVMP4AudioDecoderGetMemRequirements();
}

/* Parses the AudioSpecificConfig; returns the decoder's error code. */
extern "C" int theo_opencore_init(void *mem, const unsigned char *asc, int asc_len)
{
	memset(&ext, 0, sizeof(ext));
	ext.outputFormat = OUTPUTFORMAT_16PCM_INTERLEAVED;
	ext.aacPlusEnabled = 1;
	ext.desiredChannels = 2;
	int err = PVMP4AudioDecoderInitLibrary(&ext, mem);
	if (err != MP4AUDEC_SUCCESS)
		return err;
	ext.pInputBuffer = (UChar *)asc;
	ext.inputBufferCurrentLength = asc_len;
	ext.inputBufferMaxLength = 0;
	return PVMP4AudioDecoderConfig(&ext, mem);
}

/* Decodes one raw access unit into `pcm` (4096 samples: 2048 core plus 2048
 * for SBR); stores the bytes consumed and returns the decoder's error code. */
extern "C" int theo_opencore_decode(void *mem, const unsigned char *frame, int len,
				    short *pcm, int *used)
{
	ext.pInputBuffer = (UChar *)frame;
	ext.inputBufferCurrentLength = len;
	ext.inputBufferMaxLength = 0;
	ext.inputBufferUsedLength = 0;
	ext.remainderBits = 0;
	ext.pOutputBuffer = pcm;
	ext.pOutputBuffer_plus = pcm + 2048;
	ext.repositionFlag = false;
	int err = PVMP4AudioDecodeFrame(&ext, mem);
	/* SoftAAC: a plain LC/LTP stream decoded with SBR upsampling turns the
	 * SBR path off (AAC+ would only double the output rate). */
	if (err == MP4AUDEC_SUCCESS && ext.aacPlusUpsamplingFactor == 2
	    && (ext.extendedAudioObjectType == MP4AUDIO_AAC_LC
		|| ext.extendedAudioObjectType == MP4AUDIO_LTP))
		ext.aacPlusEnabled = 0;
	*used = ext.inputBufferUsedLength;
	return err;
}

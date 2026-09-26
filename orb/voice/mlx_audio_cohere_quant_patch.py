# Vendored from the Hugging Face model card for
#   MarkChen1214/cohere-transcribe-03-2026-MLX-Mixed-2bit3bit4bit
#   revision 553445e84959f9ec3fcd43443bce75ea05c400f3
#   file     mlx_audio_cohere_quant_patch.py (MIT, author MarkChen1214)
#   sha256   91beeed75b1c6de97c33f30a48ac508bc22eac0689ee792e8204e7dc9ad6313a
#
# Orb ships this copy so the worker never imports code straight out of the
# model cache. It is applied before mlx_audio.stt.load() and only touches the
# cohere_asr model class in the pinned mlx-audio checkout
# (77a6cfcaba9fcb246c9302f7196c05147501bd62). Reviewed: it patches sanitize()
# to be shape-aware, fixes a dtype cast in _encode_waveforms(), and swaps 1x1
# pointwise Conv1d layers for Linear so MLX can quantize them. No I/O, no
# network, no subprocesses. Keep byte-identical to upstream below this header;
# orb/voice/install.sh verifies the hash against the downloaded snapshot.
"""Runtime fixes and local extensions for Cohere Transcribe in mlx-audio.

This module currently does three things:
1. Fix `_encode_waveforms()` so quantized checkpoints do not cast frontend
   features to `uint32` before the first Conv2d.
2. Make `sanitize()` shape-aware so converted MLX checkpoints do not get
   re-transposed on reload.
3. Support an E2 experiment where Conformer 1x1 pointwise Conv1d layers are
   replaced with Linear-equivalent modules so MLX can quantize them.
"""

from __future__ import annotations

from typing import Dict

import mlx.core as mx
import mlx.nn as nn
from mlx.utils import tree_flatten

POINTWISE_LINEAR_FLAG = "codex_pointwise_linearized"


def prepare_pointwise_linearized_config(config: dict) -> dict:
    """Mark a config dict so the patched loader instantiates linearized pointwise convs."""

    updated = dict(config)
    updated[POINTWISE_LINEAR_FLAG] = True
    return updated


def _config_uses_pointwise_linears(config) -> bool:
    if isinstance(config, dict):
        return bool(config.get(POINTWISE_LINEAR_FLAG, False))
    return bool(getattr(config, POINTWISE_LINEAR_FLAG, False))


def _replace_pointwise_convs_with_linears(model) -> None:
    """Convert Conformer 1x1 Conv1d layers into Linear-equivalent modules."""

    for layer in model.encoder.layers:
        conv_module = layer.conv
        for attr_name in ("pointwise_conv1", "pointwise_conv2"):
            module = getattr(conv_module, attr_name)
            if isinstance(module, nn.Linear):
                continue
            if not isinstance(module, nn.Conv1d):
                raise TypeError(
                    f"Expected Conv1d for {attr_name}, got {type(module).__name__}"
                )
            if module.weight.shape[1] != 1:
                raise ValueError(
                    f"{attr_name} is not a 1x1 Conv1d: weight shape {module.weight.shape}"
                )
            linear = nn.Linear(
                input_dims=module.weight.shape[2],
                output_dims=module.weight.shape[0],
                bias=module.bias is not None,
            )
            setattr(conv_module, attr_name, linear)


def _patched_sanitize(self, weights: Dict[str, mx.array]) -> Dict[str, mx.array]:
    sanitized = {}
    expected_shapes = {key: value.shape for key, value in tree_flatten(self.parameters())}

    for key, value in weights.items():
        if key.startswith("preprocessor.") or key.endswith("num_batches_tracked"):
            continue

        new_key = key
        if key.startswith("transf_decoder._embedding."):
            new_key = key.replace(
                "transf_decoder._embedding.", "transf_decoder.embedding."
            )
        elif key.startswith("transf_decoder._decoder."):
            new_key = key.replace("transf_decoder._decoder.", "transf_decoder.decoder.")

        expected_shape = expected_shapes.get(new_key)
        if expected_shape == value.shape:
            sanitized[new_key] = value
            continue

        if (
            value.ndim == 3
            and new_key.endswith(("pointwise_conv1.weight", "pointwise_conv2.weight"))
            and expected_shape is not None
        ):
            # Raw HF layout: (out, in, 1) -> Linear expects (out, in)
            if value.shape[-1] == 1 and expected_shape == value.shape[:2]:
                value = mx.squeeze(value, axis=-1)
            # Already-converted MLX layout: (out, 1, in) -> Linear expects (out, in)
            elif value.shape[1] == 1 and expected_shape == (value.shape[0], value.shape[2]):
                value = mx.squeeze(value, axis=1)
        elif value.ndim == 3 and new_key.endswith("weight"):
            transposed_shape = (value.shape[0], value.shape[2], value.shape[1])
            if expected_shape == transposed_shape:
                value = mx.transpose(value, (0, 2, 1))
        elif value.ndim == 4 and new_key.endswith("weight"):
            transposed_shape = (
                value.shape[0],
                value.shape[2],
                value.shape[3],
                value.shape[1],
            )
            if expected_shape == transposed_shape:
                value = mx.transpose(value, (0, 2, 3, 1))

        sanitized[new_key] = value

    return sanitized


def _patched_encode_waveforms(self, waveforms):
    input_features, lengths = self.audio_frontend(waveforms)
    reference_weight = self.encoder.pre_encode.conv[0].weight
    if input_features.dtype != reference_weight.dtype:
        input_features = input_features.astype(reference_weight.dtype)

    encoder_hidden_states, encoder_lengths = self.encoder(input_features, lengths)
    if self.encoder_decoder_proj is not None:
        encoder_hidden_states = self.encoder_decoder_proj(encoder_hidden_states)

    encoder_mask = (
        mx.arange(encoder_hidden_states.shape[1])[None, :]
        < encoder_lengths[:, None]
    )
    return encoder_hidden_states, encoder_lengths, encoder_mask


def apply_patch() -> None:
    """Install the Cohere quantization reload fixes once per process."""

    from mlx_audio.stt.models.cohere_asr import cohere_asr
    from mlx_audio.stt.models.cohere_asr import config as cohere_config

    if getattr(cohere_asr.Model, "_codex_cohere_quant_patch", False):
        return

    original_model_init = cohere_asr.Model.__init__
    original_from_dict = cohere_config.ModelConfig.from_dict

    @classmethod
    def _patched_model_config_from_dict(cls, params):
        config = original_from_dict(params)
        if isinstance(params, dict) and params.get(POINTWISE_LINEAR_FLAG):
            setattr(config, POINTWISE_LINEAR_FLAG, True)
        return config

    def _patched_model_init(self, config):
        original_model_init(self, config)
        if _config_uses_pointwise_linears(config):
            _replace_pointwise_convs_with_linears(self)

    cohere_config.ModelConfig.from_dict = _patched_model_config_from_dict
    cohere_asr.Model.__init__ = _patched_model_init
    cohere_asr.Model.sanitize = _patched_sanitize
    cohere_asr.Model._encode_waveforms = _patched_encode_waveforms
    cohere_asr.Model._codex_cohere_quant_patch = True

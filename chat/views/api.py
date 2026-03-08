from django.http import JsonResponse
from django.views.decorators.http import require_http_methods

from ..services import fetch_available_models
from ..utils import (
    load_config, save_config, group_models_by_provider,
    flatten_models_with_provider_prefix,
)
from .core import _get_theme_list


def get_available_themes(request):
    """List available themes by scanning the themes directory"""
    themes = _get_theme_list()
    return JsonResponse({'themes': themes})


@require_http_methods(["GET"])
def get_available_models(request):
    """AJAX endpoint to fetch available models on-demand"""
    config = load_config()
    api_key = config.get("OPENROUTER_API_KEY", "")

    if not api_key:
        return JsonResponse({'error': 'No API key configured'}, status=400)

    models = fetch_available_models(api_key)
    if not models:
        return JsonResponse({'error': 'Failed to fetch models'}, status=500)

    grouped = group_models_by_provider(models)
    options = flatten_models_with_provider_prefix(grouped)
    available_models = [{'id': m[0], 'display': m[1]} for m in options]

    return JsonResponse({'models': available_models})


@require_http_methods(["POST"])
def save_theme(request):
    """Save theme preference to config.json"""
    color_theme = request.POST.get('colorTheme', 'liminal-salt')
    theme_mode = request.POST.get('themeMode', 'dark')

    config = load_config()
    config['THEME'] = color_theme
    config['THEME_MODE'] = theme_mode
    save_config(config)

    return JsonResponse({'success': True, 'theme': color_theme, 'mode': theme_mode})

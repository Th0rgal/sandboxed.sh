"""Browser fixtures for the two observed message layouts (no account/network)."""
import unittest
from scripts.chatgpt_ui_driver import USER_MESSAGE_SELECTOR, ASSISTANT_MESSAGE_SELECTOR
try:
    from playwright.sync_api import sync_playwright
except ImportError:
    sync_playwright = None

@unittest.skipUnless(sync_playwright, 'Playwright required for DOM fixture checks')
class MessageLayouts(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.pw = sync_playwright().start()
        cls.browser = cls.pw.chromium.launch(headless=True)

    @classmethod
    def tearDownClass(cls):
        cls.browser.close()
        cls.pw.stop()

    def check(self, html):
        page = self.browser.new_page()
        try:
            page.set_content('<main>'+html+'</main>')
            users = page.locator(USER_MESSAGE_SELECTOR)
            assistants = page.locator(ASSISTANT_MESSAGE_SELECTOR)
            self.assertEqual(users.count(), 1)
            self.assertEqual(assistants.count(), 1)
            self.assertEqual(users.inner_text(), 'prompt')
            self.assertEqual(assistants.inner_text(), 'answer')
        finally:
            page.close()

    def test_legacy(self):
        self.check('<div data-message-author-role="user">prompt</div><div data-message-author-role="assistant">answer</div>')

    def test_current_excludes_reasoning_and_accessibility_labels(self):
        self.check('<div><h4>You said:</h4><div class="group/user-message">prompt</div></div><div>Worked for 21s</div><div><h4 data-conversation-role="assistant">ChatGPT said:</h4><div data-chatgpt-selection-message-id="a">answer</div></div>')

    def test_wrapped_answer_and_nested_user_groups(self):
        self.check('<div class="group/user-message"><div><div class="group/user-message">prompt</div></div></div><div><h4 data-conversation-role="assistant">ChatGPT said:</h4><div><div data-chatgpt-selection-message-id="a"><div data-markdown-text-style="assistant-message">answer</div></div></div></div>')

    def test_wrapped_answer_inside_legacy_is_counted_once(self):
        self.check('<div data-message-author-role="user"><div class="group/user-message"><div class="group/user-message">prompt</div></div></div><div data-message-author-role="assistant"><h4 data-conversation-role="assistant" hidden>ChatGPT said:</h4><div><div data-chatgpt-selection-message-id="a">answer</div></div></div>')

    def test_mixed_layout_does_not_double_count_nested_nodes(self):
        self.check('<div data-message-author-role="user"><div class="group/user-message">prompt</div></div><div data-message-author-role="assistant"><h4 data-conversation-role="assistant" style="display:none">ChatGPT said:</h4><div data-chatgpt-selection-message-id="a">answer</div></div>')

@unittest.skipUnless(sync_playwright, 'Playwright required for DOM fixture checks')
class DownloadOverlay(unittest.IsolatedAsyncioTestCase):
    async def test_download_button_keyboard_activation_through_preview_overlay(self):
        import tempfile
        from pathlib import Path
        from unittest.mock import patch
        from playwright.async_api import async_playwright
        from scripts import chatgpt_ui_driver as driver
        async with async_playwright() as pw:
            browser = await pw.chromium.launch(headless=True)
            try:
                page = await browser.new_page(accept_downloads=True)
                await page.route('http://fixture.test/', lambda r: r.fulfill(status=200, body='<html></html>'))
                await page.route('http://fixture.test/orb-check.txt', lambda r: r.fulfill(status=200, content_type='text/plain', headers={'Content-Disposition':'attachment; filename=orb-check.txt'}, body='ORB_ARTIFACT_OK'))
                await page.goto('http://fixture.test/')
                await page.set_content('''<main><div id="response" style="position:relative;width:200px;height:80px">
                  <button aria-label="Download file" onclick="location.href='/orb-check.txt'">Download</button>
                  <button aria-label="Open preview of orb-check.txt" style="position:absolute;inset:0">Preview</button>
                </div></main>''')
                with tempfile.TemporaryDirectory() as directory, patch.object(driver, 'emit') as emit:
                    await driver.collect_downloads(page, page.locator('#response'), Path(directory))
                    self.assertEqual((Path(directory)/'orb-check.txt').read_text(), 'ORB_ARTIFACT_OK')
                    self.assertEqual([call.args[0] for call in emit.call_args_list], ['artifact'])
            finally:
                await browser.close()

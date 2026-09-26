import { test, expect } from '@playwright/test';

test('archive moves a controller into the shared archive and restore keeps it paused', async ({page}) => {
  let archived = false;
  const actions: string[] = [];
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname;
    let result: unknown = {};
    const view = () => ({slug:'notes',job:{id:'controller',name:'notes-controller',enabled:false,state:'paused',archived,failure_streak:0},runs:[]});
    if(path === '/api/projects') result = {projects:[{slug:'notes',title:'Project notes',status:'active'}]};
    else if(path.endsWith('/controller/action')) {
      const {action} = route.request().postDataJSON();
      actions.push(action); archived = action === 'archive'; result = view();
    } else if(path.endsWith('/controller')) result = view();
    else if(path.endsWith('/crons')) result = {jobs:[]};
    else if(path.endsWith('/missions')) result = [];
    else if(path.endsWith('/files')) result = {entries:[]};
    await route.fulfill({json:result});
  });
  const errors: string[] = [];
  page.on('pageerror', e => errors.push(e.message));
  await page.goto('/tests/browser.html?theme=dark');
  await page.getByRole('button',{name:'Project notes',exact:true}).click();
  const controller = page.getByRole('button',{name:/notes-controller/});
  await controller.click({button:'right'});
  await page.getByRole('menuitem',{name:'Archive',exact:true}).click();
  await expect(controller).toHaveCount(0);
  await page.getByRole('button',{name:'Archived',exact:true}).click();
  await page.getByRole('tree',{name:'Archived conversations'}).getByRole('button',{name:'Project notes',exact:true}).click();
  await expect(controller).toBeVisible();
  await expect(controller.getByRole('img',{name:'Paused',exact:true})).toBeVisible();
  await controller.click({button:'right'});
  await page.getByRole('menuitem',{name:'Restore',exact:true}).click();
  await expect(page.getByRole('tree',{name:'Archived conversations'}).getByRole('button',{name:/notes-controller/})).toHaveCount(0);
  await expect(controller).toBeVisible();
  expect(actions).toEqual(['archive','restore']);
  expect(errors).toEqual([]);
  await page.screenshot({path:'/tmp/orb-controller-restored.png'});
});

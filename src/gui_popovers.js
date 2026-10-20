// Menus are also populated asynchronously. Position after their content changes.
let popoverAnchor=null;
function positionFloatingMenu(){
  const menu=$('floating-menu'),context=popoverAnchor;if(!context||!menu.classList.contains('open'))return;
  if(!context.anchor?.isConnected){closeFloatingMenu();return;}
  menu.style.maxHeight='';
  const anchor=context.anchor.getBoundingClientRect(),height=window.innerHeight,width=window.innerWidth;
  const natural=menu.getBoundingClientRect(),above=Math.max(0,anchor.top-20),below=Math.max(0,height-anchor.bottom-20);
  const useAbove=context.preferAbove?(above>=natural.height||above>=below):!(below>=natural.height||below>=above);
  const space=useAbove?above:below;
  if(natural.height>space)menu.style.maxHeight=Math.max(40,Math.floor(space))+'px';
  const bounds=menu.getBoundingClientRect();
  menu.style.left=Math.max(12,Math.min(anchor.left,width-bounds.width-12))+'px';
  menu.style.top=Math.max(12,Math.min(useAbove?anchor.top-bounds.height-8:anchor.bottom+8,height-bounds.height-12))+'px';
}
const popoverBaseOpen=openFloatingMenuAt;
openFloatingMenuAt=function(key,anchor,html,preferAbove=true){if(!anchor?.isConnected){closeFloatingMenu();return;}popoverAnchor={anchor,preferAbove};popoverBaseOpen(key,anchor,html,preferAbove);positionFloatingMenu();};
const popoverBaseClose=closeFloatingMenu;
closeFloatingMenu=function(){popoverAnchor=null;popoverBaseClose();};
const popoverMutations=new MutationObserver(positionFloatingMenu);
popoverMutations.observe($('floating-menu'),{childList:true,subtree:true,characterData:true});
if(typeof ResizeObserver==='function'){const popoverSize=new ResizeObserver(positionFloatingMenu);popoverSize.observe($('floating-menu'));}
window.addEventListener('resize',positionFloatingMenu);

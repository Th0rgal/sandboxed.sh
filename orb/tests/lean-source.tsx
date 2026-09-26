import {render} from 'solid-js/web';
import {ReadOnlySource} from '../src/Markdown';
import '../src/styles.css';
render(()=><ReadOnlySource language="lean" text={'import Midnight.Import\n/-!\n# Comment with `def`\n-/\ntheorem example : True := by\n  trivial'}/>,document.getElementById('root')!);
